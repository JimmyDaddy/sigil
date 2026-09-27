use std::{
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, stream};
use sigil_kernel::{
    AgentRole, AgentRunDisposition, AgentRunOutcome, AgentRunOutput, AgentRunPurpose,
    AgentRunResult, AgentRunTerminalReason, AgentThreadId, AgentThreadStatus,
    AgentThreadStatusChangedEntry, ApprovalHandler, AssistantMessageKind, AutoApproveHandler,
    CompletionRequest, ContextBodyRef, ContextInclusionReason, ContextItem, ContextSensitivity,
    ContextSource, ContextTrustLevel, ControlEntry, ConversationRunLifecycleRecordV1,
    ConversationRunStartedEntryV1, ConversationRunTerminalStatusV1, DisclosurePresentationError,
    DisclosurePresentationReceipt, EgressDisclosurePresenter, EventHandler,
    IntegrationBaseRepresentation, IntegrationLaneCandidate, IntegrationLaneChanged,
    IntegrationLaneId, IntegrationLaneSpec, IntegrationLaneStatus, IntegrationPlan,
    IntegrationPlanId, IntegrationPlanRecorded, InteractionMode, JsonlSessionStore, MemoryConfig,
    ModelMessage, MutationEventRecorder, NoopEventHandler, PreEgressDisclosure, Provider,
    ProviderCapabilities, ProviderChunk, PublicRunEvent, PublicRunEventKind, ReasoningEffort,
    ReasoningStreamSupport, RootConfig, RunCancellationOwner, RunCancellationRequestedEntry,
    RunCancellationTarget, RunCancellationTerminalOutcome, RunEvent, RuntimeContextCandidates,
    Session, SessionLogEntry, SessionPublicEventProjectionV1, SessionRef, StartDurableTaskAction,
    StartPlanReviewAction, TERMINAL_TASK_SCHEMA_VERSION, TaskChildSessionEntry,
    TaskChildSessionStatus, TaskDirectExecutionAdmittedV1, TaskHandoffId, TaskId,
    TaskIntegrationReviewRequest, TaskPauseRequest, TaskPlanEntry, TaskPlanStatus,
    TaskRoutingPolicy, TaskRunCancellationScopeBoundEntry, TaskRunEntry, TaskRunStatus, TaskStepId,
    TaskVerificationRerunRequest, TerminalLifecycleEvent, TerminalLifecycleUpdateV2,
    TerminalReadinessKind, TerminalReadinessStatus, TerminalTaskEntry, TerminalTaskHandle,
    TerminalTaskId, TerminalTaskStatus, Tool, ToolAccess, ToolApproval, ToolArtifactSensitivity,
    ToolArtifactStore, ToolCall, ToolCategory, ToolContext, ToolExecutionEntry,
    ToolExecutionStatus, ToolPreviewCapability, ToolRegistry, ToolRegistryScope, ToolResult,
    ToolResultMeta, ToolResultRecordedV3, ToolSpec, UsageStats, UserInputActionV1,
    UserInputAnswerV1, UserInputAnswerValueV1, UserInputCommandId, UserInputContinuationBindingV1,
    UserInputDecisionAcceptedV1, UserInputDecisionV1, UserInputIdentityV1,
    UserInputLifecycleEntryV1, UserInputPurposeV1, UserInputQuestionV1, UserInputRequestId,
    UserInputRequestV1, UserInputRequestedV1, UserInputResolutionV1, UserInputSourceV1,
    UserInputStatusV1, conversation_run_lifecycle_record_from_stream,
};

use crate::agent_supervisor::task_role_runtime::TaskRoleProviderBuilder;
use crate::application_run::{
    ApplicationTaskFailed, application_task_terminal_output,
    is_application_public_outbox_append_error,
};
use sigil_tools_builtin::LocalExecutionBackend;

use super::{
    ApplicationCancellationTicket, ApplicationRunConstraints, ApplicationRunControl,
    ApplicationRunEventHandler, ApplicationRunEventSequence, ApplicationRunExecutionKind,
    ApplicationRunInteraction, ApplicationRunPrepareError, ApplicationRunPrepareErrorClass,
    ApplicationRunRequest, ApplicationRunServices, ApplicationRunTerminalStatus,
    ApplicationSessionLeaseManager, ApplicationTaskContinuationRequest,
    ApplicationTaskExecutionRuntime, ApplicationTaskPauseTicket, ApplicationTranscriptRole,
    ApplicationUserInputDecisionRequest, MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES,
    PublicApplicationEventBridge, accept_application_task_integration_review,
    admit_application_agent_binding, admit_application_model_selection,
    admit_application_reasoning_effort, admit_application_skill_binding,
    application_run_context_view, application_run_input, application_session_frontier_view,
    application_session_transcript_page, application_task_integration_review_view,
    application_terminal_projection, application_verification_view,
    attach_application_request_context, bind_application_session,
    bind_application_session_with_model, bind_application_session_with_model_ref,
    bind_application_session_with_model_ref_and_attachment, bind_existing_application_session,
    constrain_application_tool_registry, continue_application_task_handoff,
    default_application_session_path, optional_eager_mcp_warning, prepare_application_run,
    prepare_application_run_blocking, prepare_application_task_continuation,
    prepare_application_user_input_decision, record_application_preparation_cancellation,
    replay_pending_application_outbox, rerun_application_verification, validate_execution_contract,
};

#[path = "application_continuation_file_tests.rs"]
mod continuation_file_tests;

#[path = "application_write_transport_recovery_tests.rs"]
mod write_transport_recovery_tests;

#[path = "application_direct_turn_limit_tests.rs"]
mod direct_turn_limit_tests;

#[path = "application_git_five_batch_tests.rs"]
mod git_five_batch_tests;

#[path = "application_verification_guard_tests.rs"]
mod verification_guard_tests;

fn scripted_task_completion_chunks(
    _request: &CompletionRequest,
    text: &str,
    _call_id: &str,
) -> Vec<Result<ProviderChunk>> {
    vec![
        Ok(ProviderChunk::TextDelta(text.to_owned())),
        Ok(ProviderChunk::Done),
    ]
}

fn application_conversation_lifecycle(
    path: &Path,
) -> Result<Vec<ConversationRunLifecycleRecordV1>> {
    JsonlSessionStore::read_event_records(path)?
        .iter()
        .filter_map(|record| conversation_run_lifecycle_record_from_stream(record).transpose())
        .collect()
}

fn durable_application_event_sequence(
    session_id: &str,
    run_id: &str,
    path: &Path,
) -> Result<ApplicationRunEventSequence> {
    ApplicationRunEventSequence::with_outbox(
        session_id.to_owned(),
        run_id.to_owned(),
        JsonlSessionStore::new(path)?,
    )
}

fn start_application_public_control_run(session: &Session, run_id: &str) -> Result<()> {
    session
        .conversation_run_lifecycle_recorder()?
        .append_started(&ConversationRunStartedEntryV1::new(run_id, 1)?)?;
    Ok(())
}

fn application_internal_context_fixture() -> RuntimeContextCandidates {
    let body = "desktop-internal context snapshot body";
    let mut candidates = RuntimeContextCandidates::new();
    candidates.items.push(ContextItem {
        id: "application-context-fixture".to_owned(),
        source: ContextSource::RepositoryFile,
        source_event_id: None,
        trust_level: ContextTrustLevel::UntrustedRepositoryData,
        sensitivity: ContextSensitivity::Repository,
        egress_decision: None,
        repo_revision: Some("application-context-snapshot".to_owned()),
        token_cost: sigil_kernel::estimate_context_token_cost(body),
        score: Some(100.0),
        score_breakdown: Vec::new(),
        inclusion_reason: ContextInclusionReason::RetrievalHit,
        body_ref: ContextBodyRef::inline(body),
    });
    candidates
        .snippets
        .insert("application-context-fixture".to_owned(), body.to_owned());
    candidates
}

fn append_running_application_task(
    session: &mut Session,
    task_id: &TaskId,
    scope_id: Option<&str>,
    _plan_version: u32,
) -> Result<()> {
    let mut controls = vec![
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("session.jsonl")?,
            objective: "control an application Task".to_owned(),
            title: None,

            status: TaskRunStatus::Running,
            reason: None,
        }),
        ControlEntry::TaskDirectExecutionAdmittedV1(TaskDirectExecutionAdmittedV1::task_request(
            task_id.clone(),
            "control an application Task",
            1,
        )),
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

fn isolated_storage_toml(path: &Path) -> String {
    let root = path.parent().expect("test config should have a parent");
    let state_root = toml::Value::String(root.join("state").to_string_lossy().into_owned());
    let cache_root = toml::Value::String(root.join("cache").to_string_lossy().into_owned());
    format!("[storage]\nstate_root = {state_root}\ncache_root = {cache_root}\n")
}

fn write_application_test_config(path: &Path) -> Result<()> {
    let storage = isolated_storage_toml(path);
    std::fs::write(
        path,
        format!(
            r#"config_version = 2

{storage}

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
credential = {{ source = "environment", name = "SIGIL_API_KEY" }}
"#
        ),
    )?;
    Ok(())
}

fn write_unauthenticated_application_test_config(path: &Path) -> Result<()> {
    let storage = isolated_storage_toml(path);
    std::fs::write(
        path,
        format!(
            r#"config_version = 2

{storage}

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
credential = {{ source = "none" }}
"#
        ),
    )?;
    Ok(())
}

#[test]
fn application_run_preparation_applies_configured_output_default_unless_constrained() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_application_test_config(&config_path)?;
    let mut config = RootConfig::load(&config_path)?;
    config.model_request.max_output_tokens = Some(8_192);
    config.save(&config_path)?;

    let prepared = prepare_application_run_blocking(
        ApplicationRunRequest::non_interactive(&config_path, temp.path(), "hello", "default-cap"),
        Arc::new(ApplicationSessionLeaseManager::new()),
        false,
        None,
    )?;
    assert_eq!(prepared.target_max_tokens, Some(8_192));
    drop(prepared);

    let constrained = prepare_application_run_blocking(
        ApplicationRunRequest::non_interactive(&config_path, temp.path(), "hello", "explicit-cap")
            .with_constraints(ApplicationRunConstraints {
                max_turns: 1,
                max_output_tokens: 4_096,
                tool_scope: ToolRegistryScope {
                    allow_all: true,
                    ..Default::default()
                },
            }),
        Arc::new(ApplicationSessionLeaseManager::new()),
        false,
        None,
    )?;
    assert_eq!(constrained.target_max_tokens, Some(4_096));
    Ok(())
}

fn with_application_test_managed_authority(
    root: &Path,
    services: ApplicationRunServices,
) -> Result<ApplicationRunServices> {
    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        services,
        &root.join("sigil.toml"),
        root,
    )?;
    services.require_current_schema_authority()?;
    Ok(services)
}

fn bind_application_test_managed_session(
    config_path: &Path,
    root: &Path,
    requested_path: &Path,
    services: &ApplicationRunServices,
) -> Result<super::ApplicationSessionBinding> {
    let writer = Arc::clone(
        &services
            .authority_composition()
            .expect("fixture boot provides the production authority")
            .storage_writer,
    );
    let (binding, attachment) =
        crate::application_run::bind_application_session_with_model_ref_and_attachment_and_managed_writer(
            config_path,
            root,
            Some(requested_path),
            None,
            None,
            Some(writer),
        )?;
    drop(attachment);
    Ok(binding)
}

fn seed_application_user_input_request(
    config_path: &Path,
    launch_cwd: &Path,
    binding: &super::ApplicationSessionBinding,
) -> Result<UserInputRequestedV1> {
    let context = application_run_context_view(
        config_path,
        launch_cwd,
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    let store = JsonlSessionStore::new(&binding.session_log_path)?;
    let mut session = Session::load_from_store(&context.provider_name, &context.model_name, store)?;
    session.append_user_message(ModelMessage::user("implement after clarification"))?;
    let call = ToolCall {
        id: "call-user-input-runtime".to_owned(),
        name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
        args_json: r#"{"questions":[{"id":"mode","question":"Which mode should continue?"}]}"#
            .to_owned(),
    };
    let assistant = ModelMessage::assistant(None, vec![call.clone()]);
    let assistant_message_id = assistant.id.clone();
    session.append_assistant_message(assistant)?;
    let requested = UserInputRequestedV1::new(UserInputRequestV1 {
        schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
        identity: UserInputIdentityV1 {
            session_scope_id: sigil_kernel::SessionScopeId::new(&binding.session_scope_id)?,
            root_logical_run_id: sigil_kernel::LogicalRunId::new("runtime-user-input-root")?,
            source_thread_id: sigil_kernel::AgentThreadId::new("main")?,
            request_id: UserInputRequestId::new("runtime-user-input-request")?,
            generation: 1,
            source_binding_hash: format!("sha256:{}", "a".repeat(64)),
        },
        source: UserInputSourceV1::Agent,
        purpose: UserInputPurposeV1::Clarification,
        prompt: "Choose a runtime mode".to_owned(),
        questions: vec![UserInputQuestionV1 {
            id: "mode".to_owned(),
            question: "Which mode should continue?".to_owned(),
            description: None,
            required: true,
            options: Vec::new(),
            multiple: false,
        }],
        allowed_actions: vec![
            UserInputActionV1::Submit,
            UserInputActionV1::Decline,
            UserInputActionV1::CancelRun,
        ],
        requested_at_unix_ms: 10,
        continuation: Some(UserInputContinuationBindingV1 {
            assistant_message_id,
            tool_call_id: call.id.clone(),
            provider_name: context.provider_name,
            model_name: context.model_name,
        }),
    })?;
    session.append_controls(vec![
        ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: call.id,
            tool_name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
            status: ToolExecutionStatus::Started,
            duration_ms: None,
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: None,
        })),
        UserInputLifecycleEntryV1::Requested(Box::new(requested.clone())).into_control(),
    ])?;
    Ok(requested)
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Matches the SSL_CERT_FILE test env-lock pattern.
async fn submitted_user_input_is_durable_before_one_supervised_continuation() -> Result<()> {
    let _environment_guard = crate::test_env::lock();
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let services = with_application_test_managed_authority(
        temp.path(),
        ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter)),
    )?;
    let session_path = temp.path().join("state/sessions/user-input.jsonl");
    let binding =
        bind_application_test_managed_session(&config_path, temp.path(), &session_path, &services)?;
    let requested = seed_application_user_input_request(&config_path, temp.path(), &binding)?;

    let prepared = prepare_application_user_input_decision(
        ApplicationUserInputDecisionRequest {
            application_operation: None,
            config_path: config_path.clone(),
            launch_cwd: temp.path().to_path_buf(),
            session_path: binding.session_log_path.clone(),
            session_attachment: None,
            expected_session_scope_id: binding.session_scope_id.clone(),
            run_id: "user-input-continuation-1".to_owned(),
            identity: requested.request.identity.clone(),
            request_hash: requested.request_hash.clone(),
            command_id: UserInputCommandId::new("user-input-command-1")?,
            decision: UserInputDecisionV1::Submitted {
                answers: vec![UserInputAnswerV1 {
                    question_id: "mode".to_owned(),
                    value: UserInputAnswerValueV1::Text {
                        value: "safe".to_owned(),
                    },
                }],
            },
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    )
    .await?;
    assert!(prepared.has_continuation());
    assert!(prepared.receipt().continuation_required);

    let store = JsonlSessionStore::new(&binding.session_log_path)?;
    let accepted_session = Session::load_from_store("custom", "gpt-test", store.clone())?;
    let accepted = accepted_session.user_input_projection()?;
    let accepted_state = accepted
        .request(&requested.request.identity)
        .expect("accepted request should remain projected");
    assert_eq!(
        accepted_state.status,
        UserInputStatusV1::DecisionAccepted,
        "preparation must not claim continuation ownership before execution"
    );

    let (_, continuation, revision) = prepared.into_parts();
    assert!(revision.is_none());
    let (execution, control) = continuation
        .expect("submitted answer should prepare a continuation")
        .into_parts();
    let mut events = RecordingApplicationRunEvents::default();
    let mut approvals = AutoApproveHandler;
    let output = execution.execute(&mut events, &mut approvals).await?;
    assert_eq!(output.terminal_status, ApplicationRunTerminalStatus::Paused);
    assert!(matches!(
        output.agent_output.disposition,
        sigil_kernel::AgentRunDisposition::Blocked
    ));
    drop(control);

    let recovered = Session::load_from_store("custom", "gpt-test", store)?;
    let state = recovered
        .user_input_projection()?
        .request(&requested.request.identity)
        .cloned()
        .expect("continued request should remain projected");
    assert_eq!(state.status, UserInputStatusV1::Resolved);
    assert!(matches!(
        state.resolution.map(|entry| entry.resolution),
        Some(UserInputResolutionV1::Failed {
            retryable: false,
            ..
        })
    ));
    assert!(
        crate::application_run::application_recoverable_user_input_decision(
            &binding.session_log_path,
            &binding.session_scope_id,
            None,
        )?
        .is_none(),
        "an unclassified transport outcome must not replay a possibly consumed answer"
    );
    assert_eq!(
        recovered
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::ToolResultV3(result)
                    if result.call_id == "call-user-input-runtime"
            ))
            .count(),
        1,
        "the original tool call must be settled exactly once"
    );
    assert!(events.0.iter().any(|event| matches!(
        event.event,
        PublicRunEventKind::UserInputChanged {
            status: UserInputStatusV1::ContinuationStarted,
            ..
        }
    )));
    assert!(events.0.iter().any(|event| matches!(
        event.event,
        PublicRunEventKind::UserInputChanged {
            status: UserInputStatusV1::Resolved,
            ..
        }
    )));
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn submitted_user_input_remains_retryable_when_provider_preparation_fails() -> Result<()> {
    let _environment_guard = crate::test_env::lock();
    let temp = tempfile::tempdir()?;
    let _ca_bundle = crate::test_env::EnvScope::set(
        "SSL_CERT_FILE",
        temp.path().join("missing-ca-bundle.pem").as_os_str(),
    );
    let config_path = temp.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let services = with_application_test_managed_authority(
        temp.path(),
        ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter)),
    )?;
    let session_path = temp
        .path()
        .join("state/sessions/user-input-provider-failure.jsonl");
    let binding =
        bind_application_test_managed_session(&config_path, temp.path(), &session_path, &services)?;
    let requested = seed_application_user_input_request(&config_path, temp.path(), &binding)?;
    let command_id = UserInputCommandId::new("user-input-provider-failure-command")?;

    let error = match prepare_application_user_input_decision(
        ApplicationUserInputDecisionRequest {
            application_operation: None,
            config_path,
            launch_cwd: temp.path().to_path_buf(),
            session_path: binding.session_log_path.clone(),
            session_attachment: None,
            expected_session_scope_id: binding.session_scope_id.clone(),
            run_id: "user-input-provider-failure-run".to_owned(),
            identity: requested.request.identity.clone(),
            request_hash: requested.request_hash.clone(),
            command_id: command_id.clone(),
            decision: UserInputDecisionV1::Submitted {
                answers: vec![UserInputAnswerV1 {
                    question_id: "mode".to_owned(),
                    value: UserInputAnswerValueV1::Text {
                        value: "safe".to_owned(),
                    },
                }],
            },
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    )
    .await
    {
        Ok(_) => panic!("the missing explicit CA bundle must fail provider preparation"),
        Err(error) => error,
    };
    assert!(!error.to_string().is_empty());

    let recovered = Session::load_from_store(
        "custom",
        "gpt-test",
        JsonlSessionStore::new(&binding.session_log_path)?,
    )?;
    let projection = recovered.user_input_projection()?;
    let state = projection
        .request(&requested.request.identity)
        .expect("request must survive provider preparation failure");
    assert_eq!(state.status, UserInputStatusV1::Requested);
    assert!(state.decision.is_none());
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
        let chunks = if self.role == AgentRole::Planner {
            scripted_task_completion_chunks(
                &request,
                "application durable task completed",
                "application-task-completion",
            )
        } else {
            scripted_task_completion_chunks(
                &request,
                "application task step completed",
                "application-task-completion",
            )
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}

#[derive(Default)]
struct RecordingApplicationRunEvents(Vec<PublicRunEvent>);

impl ApplicationRunEventHandler for RecordingApplicationRunEvents {
    fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
        self.0.push(event);
        Ok(())
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
    let storage = isolated_storage_toml(&config_path);
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

{storage}

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
credential = {{ source = "environment", name = "SIGIL_API_KEY" }}
"#
        ),
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
        None,
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
    let services = with_application_test_managed_authority(
        temp.path(),
        ApplicationRunServices::with_session_leases(
            Arc::new(RejectingDisclosurePresenter),
            Arc::clone(&lease_manager),
        ),
    )?;
    let request = TaskVerificationRerunRequest::new(
        TaskId::new("task_1")?,
        1,
        TaskStepId::new("verify_1")?,
        "cargo-test".to_owned(),
        "check-hash".to_owned(),
        "policy-hash".to_owned(),
        Some("snapshot-1".to_owned()),
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
    let services = with_application_test_managed_authority(
        temp.path(),
        ApplicationRunServices::with_session_leases(
            Arc::new(RejectingDisclosurePresenter),
            Arc::clone(&lease_manager),
        ),
    )?;
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
    let storage = isolated_storage_toml(&config_path);
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

{storage}

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
credential = {{ source = "environment", name = "SIGIL_API_KEY" }}
"#
        ),
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
    let explicit_model = bind_application_session_with_model_ref(
        &config_path,
        temp.path(),
        Some(&temp.path().join("state/sessions/explicit-model.jsonl")),
        Some(&connection_id),
        Some("unknown-model"),
    )?;
    let explicit_context = application_run_context_view(
        &config_path,
        temp.path(),
        &explicit_model.session_log_path,
        &explicit_model.session_scope_id,
    )?;
    assert_eq!(explicit_context.model_ref.connection_id, connection_id);
    assert_eq!(explicit_context.model_name, "unknown-model");

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
    let storage = isolated_storage_toml(&config_path);
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

{storage}

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
credential = {{ source = "environment", name = "SIGIL_API_KEY" }}

[connections.deepseek-team]
label = "DeepSeek team"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = {{ source = "environment", name = "SIGIL_API_KEY" }}
"#
        ),
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
    let storage = isolated_storage_toml(&config_path);
    let write_config = |port: u16| -> Result<()> {
        std::fs::write(
            &config_path,
            format!(
                r#"config_version = 2

{storage}

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
        None,
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
        None,
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
        None,
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
    let storage = isolated_storage_toml(&config_path);
    let write_config = |port: u16| -> Result<()> {
        std::fs::write(
            &config_path,
            format!(
                r#"config_version = 2

{storage}

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
    let storage = isolated_storage_toml(&config_path);
    let write_config = |path: &str| -> Result<()> {
        std::fs::write(
            &config_path,
            format!(
                r#"config_version = 2

{storage}

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
        None,
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
    assert_eq!(empty.model_options.len(), 3);
    let vision = empty
        .model_options
        .iter()
        .find(|option| option.model_name == "deepseek-v4-flash-vision-exp")
        .expect("vision model option");
    assert_eq!(
        vision.availability,
        crate::provider_connections::ModelAvailability::Unverified
    );
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
    assert_eq!(empty.cache_usage, None);
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
            cache_usage: Some(sigil_kernel::CacheUsageV1 {
                schema_version: sigil_kernel::CacheUsageV1::SCHEMA_VERSION,
                read: Some(sigil_kernel::CacheTokenCountV1::provider_reported(30_000)),
                write: Some(sigil_kernel::CacheTokenCountV1::provider_reported(2_000)),
                uncached: Some(sigil_kernel::CacheTokenCountV1::provider_reported(12_000)),
                local_layout_mutation: Some(
                    sigil_kernel::CacheLayoutMutationKind::ConversationTailAppended,
                ),
                provider_miss_without_local_mutation: false,
            }),
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
    assert_eq!(
        used.cache_usage,
        Some(super::ApplicationCacheUsageView {
            cache_read_tokens: 30_000,
            cache_miss_tokens: 12_000,
            cache_write_tokens: Some(2_000),
            last_layout_mutation: Some(
                sigil_kernel::CacheLayoutMutationKind::ConversationTailAppended,
            ),
            provider_miss_without_local_mutation: false,
        })
    );
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
    let (selected_provider, selected_route) =
        admit_application_model_selection(&request, &root_config, &session)?
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
            None,
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
        None,
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
        admit_application_model_selection(&stale, &root_config, &pro_session,),
        Err(ApplicationRunPrepareError::InvalidInvocation { .. })
    ));
    Ok(())
}

#[test]
fn run_model_selection_switches_provider_connections_without_replacing_the_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let storage = isolated_storage_toml(&config_path);
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

{storage}

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
credential = {{ source = "none" }}

[connections.deepseek-team]
label = "DeepSeek team"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = {{ source = "environment", name = "SIGIL_API_KEY" }}
"#
        ),
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
        None,
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
    let storage = isolated_storage_toml(&config_path);
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

{storage}

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
credential = {{ source = "none" }}
"#
        ),
    )?;
    let session_path = temp.path().join("state/sessions/replacement-binding.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

{storage}

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
credential = {{ source = "none" }}
"#
        ),
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
            None,
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
        None,
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
    let state_root = temp.path().join("state");
    let cache_root = temp.path().join("cache");
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

[storage]
state_root = {}
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
            toml::Value::String(state_root.to_string_lossy().into_owned()),
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
    // A refreshed catalog can omit a usable configured alias and change unrelated metadata.
    // It must not invalidate the already displayed source-route selection.
    crate::provider_connections::seed_unauthenticated_catalog_cache_for_test(
        &cache_root,
        connection,
        &[],
    )?;
    let refreshed = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    assert_eq!(
        context.model_selection_binding,
        refreshed.model_selection_binding
    );
    assert_eq!(
        refreshed.model_options[0].availability,
        crate::provider_connections::ModelAvailability::Unverified
    );
    let mut request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "use this model alias",
        "run-unlisted-model",
    );
    request.session_path = Some(binding.session_log_path.clone());
    request.model_connection_id = Some(connection.id.clone());
    request.model_name = Some("unlisted-alias".to_owned());
    request.model_selection_binding = Some(context.model_selection_binding);
    let prepared = prepare_application_run_blocking(
        request,
        Arc::new(ApplicationSessionLeaseManager::new()),
        false,
        None,
    )?;
    assert_eq!(prepared.session.model_name(), "unlisted-alias");
    drop(prepared);
    let manual = bind_application_session_with_model_ref(
        &config_path,
        temp.path(),
        None,
        Some(&connection.id),
        Some("another-unlisted-alias"),
    )?;
    assert!(!manual.session_scope_id.is_empty());
    assert!(
        bind_application_session_with_model_ref(
            &config_path,
            temp.path(),
            None,
            Some(&sigil_kernel::ConnectionId::new("missing-connection")?),
            Some("unlisted-alias"),
        )
        .is_err()
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

#[tokio::test]
async fn inline_skill_image_only_preparation_preserves_images_and_exact_skill_context() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let config = std::fs::read_to_string(&config_path)?;
    std::fs::write(
        &config_path,
        config
            .replace("gpt-test", "gpt-4.1")
            .replace("chat_completions", "responses"),
    )?;
    let skill_path = temp.path().join(".sigil/skills/review-image/SKILL.md");
    std::fs::create_dir_all(skill_path.parent().expect("skill parent"))?;
    std::fs::write(
        &skill_path,
        "---\nname: review-image\ndescription: Review attached image.\ntrust: trusted\nrun-as: inline\nuser-invocable: true\n---\nInspect the attached screenshot and report concrete findings.\n",
    )?;
    let config = RootConfig::load(&config_path)?;
    let catalog = crate::application_extension_catalog_view(&config, temp.path(), &[])?;
    let binding = catalog
        .skills
        .iter()
        .find(|skill| skill.id == "review-image")
        .expect("image skill")
        .binding
        .clone()
        .expect("available skill binding");
    let paths = crate::resolve_sigil_paths(&config.storage, &config.session, temp.path());
    let cache = crate::ControlledImageAttachmentCache::new(paths.attachments_root);
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 3).write_to(&mut encoded, image::ImageFormat::Png)?;
    let image = cache
        .ingest_encoded_bytes("skill-image", encoded.into_inner())?
        .without_resolved_bytes();
    let mut request =
        ApplicationRunRequest::non_interactive(&config_path, temp.path(), "", "inline-skill-image");
    request.skill_binding = Some(binding);
    request.image_attachments = vec![image.clone()];
    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter)),
        &config_path,
        temp.path(),
    )?;
    let prepared = prepare_application_run(request, &services).await?;
    let ApplicationRunExecutionKind::Main { input, .. } = &prepared.execution.kind else {
        panic!("inline skill remains in the foreground conversation");
    };
    assert_eq!(input.persisted_image_attachments, vec![image]);
    assert_eq!(input.persisted_user_message.as_deref(), Some(""));
    assert!(input.transient_context.iter().any(|context| {
        context
            .content
            .as_deref()
            .is_some_and(|text| text.contains("Inspect the attached screenshot"))
    }));
    assert!(prepared.execution.session.entries().iter().any(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::SkillLoaded(loaded)) if loaded.skill_id == "review-image")));
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

#[tokio::test]
async fn builtin_plan_agent_binding_prepares_an_unstarted_explicit_review() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let root_config = RootConfig::load(&config_path)?;
    let catalog = crate::application_extension_catalog_view(&root_config, temp.path(), &[])?;
    let plan = catalog
        .agents
        .iter()
        .find(|agent| agent.id == "plan" && agent.available)
        .expect("the built-in plan agent should be available");
    let mut request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "inspect the runtime before implementation",
        "run-explicit-plan-review",
    );
    request.agent_binding = plan.binding.clone();
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter));

    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        services,
        &config_path,
        temp.path(),
    )?;
    services.require_current_schema_authority()?;
    let prepared = prepare_application_run(request, &services).await?;

    assert!(matches!(
        prepared.execution.kind,
        ApplicationRunExecutionKind::ExplicitPlanReview { .. }
    ));
    let projection =
        sigil_kernel::PlanReviewProjection::from_entries(prepared.execution.session.entries());
    assert!(
        projection.reviews().next().is_none(),
        "prepare must only bind the explicit review; its executor owns the first Started append"
    );
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
    let (recorded, _) = ToolResultRecordedV3::capture(
        &ToolResult::ok(
            "call-1",
            "read_file",
            "tool output",
            ToolResultMeta::default(),
        ),
        Some(&artifact_store),
        ToolArtifactSensitivity::Ordinary,
    )?;
    store.append(&SessionLogEntry::ToolResultV3(recorded))?;
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
fn application_transcript_hides_provider_visible_context_v2_snapshots() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_application_test_config(&config_path)?;
    let session_path = temp.path().join("state/sessions/context-transcript.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    let store = JsonlSessionStore::new(&binding.session_log_path)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session.append_user_message(ModelMessage::user("inspect the transcript contract"))?;
    session.build_request_with_transient_messages_and_context(
        temp.path(),
        &MemoryConfig::with_enabled(false),
        Vec::new(),
        None,
        None,
        None,
        &[],
        application_internal_context_fixture(),
    )?;
    session.append_assistant_message(ModelMessage::assistant_with_kind(
        Some("done".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    ))?;

    let page = application_session_transcript_page(
        &binding.session_log_path,
        &binding.session_scope_id,
        None,
        10,
    )?;
    assert_eq!(page.total_messages, 2);
    assert!(!format!("{page:?}").contains("desktop-internal context snapshot body"));
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
#[ignore = "opt-in R70.4 cold-cache qualification workload"]
fn cold_cache_transcript_page_100k_keeps_the_resident_page_bounded() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_application_test_config(&config_path)?;
    let session_path = temp.path().join("state/sessions/cold-cache-100k.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    let store = JsonlSessionStore::new(&binding.session_log_path)?;
    for index in 0..100_000 {
        store.append(&SessionLogEntry::User(ModelMessage::user(format!(
            "cold-cache-message-{index}"
        ))))?;
    }

    let started = std::time::Instant::now();
    let page = application_session_transcript_page(
        &binding.session_log_path,
        &binding.session_scope_id,
        None,
        32,
    )?;
    let elapsed_ms = started.elapsed().as_millis();
    assert_eq!(page.total_messages, 100_000);
    assert_eq!(page.messages.len(), 32);
    assert_eq!(
        page.messages.first().map(|message| message.ordinal),
        Some(99_969)
    );
    assert_eq!(
        page.messages.last().map(|message| message.ordinal),
        Some(100_000)
    );
    assert_eq!(page.next_before, Some(99_969));
    assert!(page.messages.iter().all(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|text| text.len() < 128)
    }));
    eprintln!(
        "r70 cold-cache transcript 100k: page={} resident_messages={} elapsed_ms={elapsed_ms}",
        page.total_messages,
        page.messages.len()
    );
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
fn public_event_bridge_rejects_a_root_terminal_without_a_durable_finalizer() -> Result<()> {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let mut recorder = Recorder::default();
    let session = Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&path)?)?;
    let events = durable_application_event_sequence(session.session_scope_id(), "run-1", &path)?;
    let mut bridge = PublicApplicationEventBridge::new(events, &mut recorder)?;
    bridge.emit(PublicRunEventKind::RunStarted {
        prompt: "hello".to_owned(),
    })?;
    sigil_kernel::EventHandler::handle(&mut bridge, RunEvent::TextDelta("hi".to_owned()))?;
    assert!(
        bridge
            .emit(PublicRunEventKind::RunFinished {
                final_text: "hi".to_owned(),
            })
            .is_err()
    );
    drop(bridge);

    assert_eq!(
        recorder
            .0
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert!(matches!(
        recorder.0[0].event,
        PublicRunEventKind::RunStarted { .. }
    ));
    assert_eq!(recorder.0.len(), 1, "live previews cannot be public events");
    assert!(
        recorder
            .0
            .iter()
            .all(|event| { !matches!(event.event, PublicRunEventKind::RunFinished { .. }) })
    );
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

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let mut recorder = Recorder::default();
    let session = Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&path)?)?;
    let events = durable_application_event_sequence(session.session_scope_id(), "run-1", &path)?;
    let mut bridge = PublicApplicationEventBridge::new(events, &mut recorder)?;
    sigil_kernel::EventHandler::handle(
        &mut bridge,
        RunEvent::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: TaskId::new("task-1")?,
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "private task objective".to_owned(),
            title: None,

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
fn public_event_bridge_marks_invalid_raw_user_input_projection_without_terminalizing() -> Result<()>
{
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let source_path = temp.path().join("state/sessions/source-user-input.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&source_path))?;
    let requested = seed_application_user_input_request(&config_path, temp.path(), &binding)?;
    let accepted = UserInputDecisionAcceptedV1::new(
        &requested,
        UserInputCommandId::new("invalid-raw-public-answer")?,
        UserInputDecisionV1::Submitted {
            answers: vec![UserInputAnswerV1 {
                question_id: "mode".to_owned(),
                value: UserInputAnswerValueV1::Text {
                    value: "private answer value".to_owned(),
                },
            }],
        },
        20,
    )?;

    let target_path = temp.path().join("target-session.jsonl");
    let target =
        Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&target_path)?)?;
    let mut recorder = Recorder::default();
    let events = durable_application_event_sequence(
        target.session_scope_id(),
        "run-raw-invalid",
        &target_path,
    )?;
    let mut bridge = PublicApplicationEventBridge::new(events, &mut recorder)?;
    let error = EventHandler::handle(
        &mut bridge,
        RunEvent::Control(
            UserInputLifecycleEntryV1::DecisionAccepted(Box::new(accepted)).into_control(),
        ),
    )
    .expect_err("a raw answer without its durable request prefix must not be projected");
    assert!(is_application_public_outbox_append_error(&error));
    drop(bridge);

    assert!(recorder.0.is_empty());
    let records = JsonlSessionStore::read_event_records(&target_path)?;
    assert!(records.iter().all(|record| {
        record.stored_event().event_kind() != Some(sigil_kernel::DurableEventType::RunFinalized)
    }));
    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    assert!(
        projection
            .events_in_order()
            .iter()
            .all(|entry| { !matches!(entry.event.event, PublicRunEventKind::RunFailed { .. }) })
    );
    Ok(())
}

#[test]
fn public_control_commit_bundles_typed_controls_with_their_exact_domain_ids() -> Result<()> {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let mut session =
        Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&path)?)?;
    start_application_public_control_run(&session, "run-control")?;
    let mut recorder = Recorder::default();
    let events =
        durable_application_event_sequence(session.session_scope_id(), "run-control", &path)?;
    let mut bridge = PublicApplicationEventBridge::new(events, &mut recorder)?;
    let domain = EventHandler::commit_controls(
        &mut bridge,
        &mut session,
        vec![
            ControlEntry::TaskRun(TaskRunEntry {
                task_id: TaskId::new("task-control")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "private task objective".to_owned(),
                title: None,
                status: TaskRunStatus::Running,
                reason: None,
            }),
            ControlEntry::TaskPlan(TaskPlanEntry {
                task_id: TaskId::new("task-control")?,
                plan_version: 1,
                status: TaskPlanStatus::Accepted,
                steps: Vec::new(),
                reason: None,
            }),
        ],
    )?;
    drop(bridge);

    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &JsonlSessionStore::read_event_records(&path)?,
    )?;
    let public = projection.events_in_order();
    assert_eq!(domain.len(), 2);
    assert_eq!(public.len(), 2, "accepted TaskPlan emits one typed DTO");
    assert_eq!(public[0].domain_event_id, domain[0].event_id);
    assert_eq!(public[1].domain_event_id, domain[1].event_id);
    assert_eq!(
        public
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(matches!(
        &public[0].event.event,
        PublicRunEventKind::TaskPhaseChanged {
            task_id: Some(task_id),
            status,
            ..
        } if task_id == "task-control" && status == "running"
    ));
    assert!(matches!(
        &public[1].event.event,
        PublicRunEventKind::TaskPlanUpdated { task_id, .. }
            if task_id == "task-control"
    ));
    assert_eq!(recorder.0.len(), 2);
    Ok(())
}

#[test]
fn public_session_commit_bundles_source_private_companion_and_outbox_atomically() -> Result<()> {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let mut session =
        Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&path)?)?;
    start_application_public_control_run(&session, "run-session-publication")?;
    let mut recorder = Recorder::default();
    let events = durable_application_event_sequence(
        session.session_scope_id(),
        "run-session-publication",
        &path,
    )?;
    let mut bridge = PublicApplicationEventBridge::new(events, &mut recorder)?;
    let assistant = ModelMessage::assistant(Some("bounded public answer".to_owned()), Vec::new());
    let domain = EventHandler::commit_session_publications(
        &mut bridge,
        &mut session,
        vec![
            SessionLogEntry::Assistant(assistant.clone()),
            SessionLogEntry::Control(ControlEntry::Note {
                kind: "private_publication_companion".to_owned(),
                data: serde_json::json!({"secret": "must not leave the durable domain log"}),
            }),
        ],
        vec![SessionPublicEventProjectionV1::assistant_message(
            0, assistant,
        )],
    )?;
    drop(bridge);

    let records = JsonlSessionStore::read_event_records(&path)?;
    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    let public = projection.events_in_order();
    assert_eq!(domain.len(), 2);
    assert_eq!(public.len(), 1);
    assert_eq!(public[0].domain_event_id, domain[0].event_id);
    assert_eq!(public[0].sequence, 1);
    assert!(matches!(
        &public[0].event.event,
        PublicRunEventKind::AssistantMessage { message }
            if message.content.as_deref() == Some("bounded public answer")
    ));
    let source_position = records
        .iter()
        .position(|record| record.stored_event().event_id == domain[0].event_id)
        .expect("assistant source record");
    let public_position = records
        .iter()
        .position(|record| record.stored_event().event_id == public[0].public_event_id)
        .expect("public outbox record");
    assert_eq!(public_position, source_position + 1);
    assert!(!serde_json::to_string(&public)?.contains("must not leave"));
    assert_eq!(recorder.0.len(), 1);
    Ok(())
}

#[test]
fn public_control_commit_keeps_private_controls_private_but_retains_tool_and_agent_activity()
-> Result<()> {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let mut session =
        Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&path)?)?;
    start_application_public_control_run(&session, "run-private-control")?;
    let mut recorder = Recorder::default();
    let events = durable_application_event_sequence(
        session.session_scope_id(),
        "run-private-control",
        &path,
    )?;
    let mut bridge = PublicApplicationEventBridge::new(events, &mut recorder)?;
    let domain = EventHandler::commit_controls(
        &mut bridge,
        &mut session,
        vec![
            ControlEntry::Note {
                kind: "private_audit".to_owned(),
                data: serde_json::json!({"secret": "do not publish"}),
            },
            ControlEntry::TaskChildSession(TaskChildSessionEntry {
                task_id: TaskId::new("task-private")?,
                plan_version: 1,
                step_id: TaskStepId::new("step-private")?,
                child_task_id: TaskId::new("child-private")?,
                child_session_ref: SessionRef::new_relative("children/private-child.jsonl")?,
                role: AgentRole::SubagentRead,
                status: TaskChildSessionStatus::Started,
                summary_hash: None,
            }),
            ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
                call_id: "call-public-tool".to_owned(),
                tool_name: "read_file".to_owned(),
                status: ToolExecutionStatus::Started,
                duration_ms: None,
                subjects: Vec::new(),
                changed_files: Vec::new(),
                metadata: ToolResultMeta::default(),
                error: None,
                model_content_hash: None,
            })),
            ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                thread_id: AgentThreadId::new("agent-private")?,
                status: AgentThreadStatus::Running,
                reason: Some("private execution detail".to_owned()),
                updated_at_ms: Some(1),
            }),
        ],
    )?;
    drop(bridge);

    assert_eq!(
        domain.len(),
        4,
        "private controls are still durable domain facts"
    );
    assert_eq!(recorder.0.len(), 2);
    assert!(matches!(
        &recorder.0[0].event,
        PublicRunEventKind::Control { control }
            if control.kind == "tool_execution" && control.payload.is_some()
    ));
    assert!(matches!(
        &recorder.0[1].event,
        PublicRunEventKind::Control { control }
            if control.kind == "agent_thread_status_changed" && control.payload.is_none()
    ));
    let public_json = serde_json::to_string(&recorder.0)?;
    assert!(!public_json.contains("do not publish"));
    assert!(!public_json.contains("private-child.jsonl"));
    assert!(!public_json.contains("private execution detail"));
    Ok(())
}

#[test]
fn public_control_commit_replays_a_failed_delivery_before_a_later_control() -> Result<()> {
    struct FailFirstAdapter {
        failed: bool,
        events: Vec<PublicRunEvent>,
    }

    impl ApplicationRunEventHandler for FailFirstAdapter {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            if !self.failed {
                self.failed = true;
                anyhow::bail!("adapter is temporarily unavailable");
            }
            self.events.push(event);
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingAdapter(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for RecordingAdapter {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let mut session =
        Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&path)?)?;
    start_application_public_control_run(&session, "run-control-replay")?;
    let events = durable_application_event_sequence(
        session.session_scope_id(),
        "run-control-replay",
        &path,
    )?;
    let mut failing = FailFirstAdapter {
        failed: false,
        events: Vec::new(),
    };
    let mut bridge = PublicApplicationEventBridge::new(events.clone(), &mut failing)?;
    EventHandler::commit_controls(
        &mut bridge,
        &mut session,
        vec![ControlEntry::TaskRun(TaskRunEntry {
            task_id: TaskId::new("task-control-replay")?,
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "durable control".to_owned(),
            title: None,
            status: TaskRunStatus::Running,
            reason: None,
        })],
    )?;
    drop(bridge);
    assert!(events.delivery_is_degraded()?);
    assert!(failing.events.is_empty());

    let mut replay = RecordingAdapter::default();
    let mut bridge = PublicApplicationEventBridge::new(
        durable_application_event_sequence(
            session.session_scope_id(),
            "run-control-replay",
            &path,
        )?,
        &mut replay,
    )?;
    EventHandler::commit_controls(
        &mut bridge,
        &mut session,
        vec![ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: TaskId::new("task-control-replay")?,
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: Vec::new(),
            reason: None,
        })],
    )?;
    drop(bridge);

    assert_eq!(
        replay
            .0
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2],
        "the failed control is replayed before the later accepted TaskPlan batch"
    );
    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &JsonlSessionStore::read_event_records(&path)?,
    )?;
    assert!(projection.pending_for_adapter("application").is_empty());
    Ok(())
}

#[test]
fn public_control_commit_reopens_user_input_projection_without_answer_values() -> Result<()> {
    check_reopened_user_input_projection(false)
}

#[test]
fn deferred_recorder_public_control_commit_reopens_user_input_projection_without_answer_values()
-> Result<()> {
    check_reopened_user_input_projection(true)
}

fn check_reopened_user_input_projection(deferred: bool) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let session_path = temp
        .path()
        .join("state/sessions/user-input-public-control.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    let store = JsonlSessionStore::new(&binding.session_log_path)?;
    let started = Session::load_from_store("custom", "gpt-test", store.clone())?;
    start_application_public_control_run(&started, "runtime-user-input-root")?;
    drop(started);
    let requested = seed_application_user_input_request(&config_path, temp.path(), &binding)?;
    let mut session = Session::load_from_store("custom", "gpt-test", store)?;
    let accepted = UserInputDecisionAcceptedV1::new(
        &requested,
        UserInputCommandId::new("public-control-answer")?,
        UserInputDecisionV1::Submitted {
            answers: vec![UserInputAnswerV1 {
                question_id: "mode".to_owned(),
                value: UserInputAnswerValueV1::Text {
                    value: "private answer value".to_owned(),
                },
            }],
        },
        20,
    )?;
    let published = commit_reopened_application_controls(
        &mut session,
        "runtime-user-input-root",
        vec![UserInputLifecycleEntryV1::DecisionAccepted(Box::new(accepted)).into_control()],
        deferred,
    )?;

    assert!(matches!(
        published.as_slice(),
        [PublicRunEvent {
            event: PublicRunEventKind::UserInputChanged {
                status: UserInputStatusV1::DecisionAccepted,
                ..
            },
            ..
        }]
    ));
    assert!(!serde_json::to_string(&published)?.contains("private answer value"));
    Ok(())
}

#[test]
fn public_control_commit_reopens_integration_context_before_projecting_a_lane() -> Result<()> {
    check_reopened_integration_projection(false)
}

#[test]
fn deferred_recorder_public_control_commit_reopens_integration_context_before_projecting_a_lane()
-> Result<()> {
    check_reopened_integration_projection(true)
}

fn check_reopened_integration_projection(deferred: bool) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let mut session =
        Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&path)?)?;
    start_application_public_control_run(&session, "run-integration-context")?;
    let plan_id = IntegrationPlanId::new("integration-public-plan")?;
    let lane_id = IntegrationLaneId::new("integration-public-lane")?;
    session.append_control(ControlEntry::IntegrationPlanRecorded(
        IntegrationPlanRecorded {
            plan: IntegrationPlan {
                plan_id: plan_id.clone(),
                task_id: TaskId::new("task-integration-context")?,
                plan_version: 3,
                base_snapshot_id: "snapshot-public".to_owned(),
                base_representation: IntegrationBaseRepresentation::Unknown,
                proposals: Vec::new(),
                conflicts: Vec::new(),
                lanes: vec![IntegrationLaneSpec {
                    lane_id: lane_id.clone(),
                    proposals: Vec::new(),
                    verification_scope_hashes: Vec::new(),
                }],
            },
        },
    ))?;
    let published = commit_reopened_application_controls(
        &mut session,
        "run-integration-context",
        vec![ControlEntry::IntegrationLaneChanged(
            IntegrationLaneChanged {
                plan_id,
                lane_id,
                status: IntegrationLaneStatus::Ready,
                candidate: Some(IntegrationLaneCandidate::ManagedRef {
                    private_ref: "refs/private/integration-lane".to_owned(),
                    base_commit: "b".repeat(40),
                    candidate_commit: "a".repeat(40),
                    workspace_snapshot_id: "snapshot-private".to_owned(),
                }),
                verification_check_ids: vec!["check-public".to_owned()],
                reason: None,
            },
        )],
        deferred,
    )?;

    assert!(matches!(
        published.as_slice(),
        [PublicRunEvent {
            event: PublicRunEventKind::IntegrationLaneChanged {
                task_id,
                status,
                ..
            },
            ..
        }] if task_id == "task-integration-context" && status == "ready"
    ));
    assert!(!serde_json::to_string(&published)?.contains("refs/private/integration-lane"));
    Ok(())
}

fn commit_reopened_application_controls(
    session: &mut Session,
    run_id: &str,
    controls: Vec<ControlEntry>,
    deferred: bool,
) -> Result<Vec<PublicRunEvent>> {
    let path = session
        .store_path()
        .ok_or_else(|| anyhow::anyhow!("missing fixture store"))?
        .to_path_buf();
    if deferred {
        let mut recorder = crate::ApplicationRunEventRecorder::resume(session, run_id)?;
        let before = recorder.public_sequence()?;
        EventHandler::commit_controls(&mut recorder, session, controls)?;
        let records = JsonlSessionStore::read_event_records(&path)?;
        let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
        let published = projection
            .events_in_order()
            .into_iter()
            .filter(|entry| entry.run_id == run_id && entry.sequence > before)
            .collect::<Vec<_>>();
        assert!(!published.is_empty());
        for entry in &published {
            assert!(
                projection
                    .pending_for_adapter("application")
                    .iter()
                    .any(|pending| pending.public_event_id == entry.public_event_id),
                "a deferred recorder cannot acknowledge application delivery"
            );
        }
        Ok(published
            .into_iter()
            .map(|entry| entry.event.clone())
            .collect())
    } else {
        let mut sink = RecordingApplicationRunEvents::default();
        let events = durable_application_event_sequence(session.session_scope_id(), run_id, &path)?;
        let mut bridge = PublicApplicationEventBridge::new(events, &mut sink)?;
        EventHandler::commit_controls(&mut bridge, session, controls)?;
        drop(bridge);
        Ok(sink.0)
    }
}

#[test]
fn deferred_recorder_resume_preserves_public_sequence_terminal_and_pending_delivery() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("recorder-resume.jsonl");
    let mut session = Session::load_from_store("fixture", "model", JsonlSessionStore::new(&path)?)?;
    let run_id = "recorder-resume-root";
    let first = crate::ApplicationRunEventRecorder::start(&session, run_id, "inspect")?;
    assert_eq!(first.public_sequence()?, 1);
    drop(first);
    let prefix = std::fs::read(&path)?;
    let mut resumed = crate::ApplicationRunEventRecorder::resume(&session, run_id)?;
    assert_eq!(
        std::fs::read(&path)?,
        prefix,
        "resume must not emit or acknowledge events"
    );
    assert_eq!(resumed.public_sequence()?, 1);
    let message = ModelMessage::assistant(Some("current output".to_owned()), Vec::new());
    EventHandler::commit_session_publications(
        &mut resumed,
        &mut session,
        vec![SessionLogEntry::Assistant(message.clone())],
        vec![SessionPublicEventProjectionV1::assistant_message(
            0, message,
        )],
    )?;
    assert_eq!(resumed.public_sequence()?, 2);
    resumed.finish_blocked(false, "fixture blocked")?;
    assert_eq!(resumed.public_sequence()?, 3);
    drop(resumed);
    let terminal_bytes = std::fs::read(&path)?;
    let mut terminal = crate::ApplicationRunEventRecorder::resume(&session, run_id)?;
    assert_eq!(terminal.public_sequence()?, 3);
    assert!(terminal.live_preview_source().is_terminal());
    assert!(terminal.begin_live_attempt("late-attempt").is_err());
    terminal.finish_blocked(false, "must not replace the original terminal")?;
    assert_eq!(std::fs::read(&path)?, terminal_bytes);
    let records = JsonlSessionStore::read_event_records(&path)?;
    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    assert_eq!(
        projection
            .events_in_order()
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(projection.pending_for_adapter("application").len(), 3);
    assert_eq!(application_conversation_lifecycle(&path)?.len(), 2);
    Ok(())
}

#[test]
fn deferred_recorder_resume_retains_existing_stream_recovery_boundaries() -> Result<()> {
    use std::io::Write;
    for damage in ["partial_tail", "checksum", "missing", "empty"] {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("recorder-recovery.jsonl");
        let session = Session::load_from_store("fixture", "model", JsonlSessionStore::new(&path)?)?;
        drop(crate::ApplicationRunEventRecorder::start(
            &session,
            "recovery-run",
            "inspect",
        )?);
        let original = std::fs::read(&path)?;
        match damage {
            "partial_tail" => std::fs::OpenOptions::new()
                .append(true)
                .open(&path)?
                .write_all(b"{\"incomplete\":")?,
            "checksum" => {
                let mut lines = String::from_utf8(original.clone())?
                    .lines()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                let mut first: serde_json::Value = serde_json::from_str(&lines[0])?;
                first["record_checksum"] = "0".repeat(64).into();
                lines[0] = serde_json::to_string(&first)?;
                std::fs::write(&path, format!("{}\n", lines.join("\n")))?;
            }
            "missing" => std::fs::remove_file(&path)?,
            "empty" => std::fs::write(&path, [])?,
            _ => unreachable!(),
        }
        let before = std::fs::read(&path).ok();
        let resumed = crate::ApplicationRunEventRecorder::resume(&session, "recovery-run");
        if damage == "partial_tail" {
            assert_eq!(resumed?.public_sequence()?, 1);
            assert!(std::fs::read(&path)?.starts_with(&original));
            let records = JsonlSessionStore::read_event_records(&path)?;
            assert!(
                records
                    .iter()
                    .any(|record| record.stored_event().event_kind()
                        == Some(sigil_kernel::DurableEventType::LogTailRecovered))
            );
        } else {
            assert!(
                resumed.is_err(),
                "{damage} must not be treated as an empty new stream"
            );
            assert_eq!(
                std::fs::read(&path).ok(),
                before,
                "rejected {damage} must remain unchanged"
            );
        }
    }
    Ok(())
}

#[test]
fn public_control_commit_rejects_a_foreign_durable_session_before_appending() -> Result<()> {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let foreign_path = temp.path().join("foreign-session.jsonl");
    let session = Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&path)?)?;
    start_application_public_control_run(&session, "run-foreign")?;
    let mut foreign =
        Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&foreign_path)?)?;
    let foreign_entry_count = foreign.entries().len();
    let foreign_bytes = std::fs::read(&foreign_path)?;
    let parent_bytes = std::fs::read(&path)?;
    let mut recorder = Recorder::default();
    let events =
        durable_application_event_sequence(session.session_scope_id(), "run-foreign", &path)?;
    let mut bridge = PublicApplicationEventBridge::new(events, &mut recorder)?;
    let error = EventHandler::commit_controls(
        &mut bridge,
        &mut foreign,
        vec![ControlEntry::Note {
            kind: "private_audit".to_owned(),
            data: serde_json::json!({"secret": "must stay foreign"}),
        }],
    )
    .expect_err("a bridge must not write another session");
    drop(bridge);

    assert!(error.to_string().contains("another durable session"));
    assert_eq!(foreign.entries().len(), foreign_entry_count);
    assert_eq!(std::fs::read(&foreign_path)?, foreign_bytes);
    assert_eq!(std::fs::read(&path)?, parent_bytes);
    assert!(recorder.0.is_empty());
    Ok(())
}

#[test]
fn public_event_sequence_rejects_a_root_terminal_without_a_durable_finalizer() -> Result<()> {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let session = Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&path)?)?;
    let sequence = durable_application_event_sequence(session.session_scope_id(), "run-1", &path)?;
    let mut recorder = Recorder::default();
    sequence.emit(
        &mut recorder,
        PublicRunEventKind::RunStarted {
            prompt: "hello".to_owned(),
        },
    )?;
    assert!(
        sequence
            .emit(
                &mut recorder,
                PublicRunEventKind::RunFailed {
                    error: "interrupted".to_owned(),
                },
            )
            .is_err()
    );
    assert!(
        sequence
            .emit(
                &mut recorder,
                PublicRunEventKind::TextDelta {
                    text: "late".to_owned(),
                },
            )
            .is_ok()
    );
    assert_eq!(
        recorder.0.len(),
        1,
        "transient previews are not public events"
    );
    Ok(())
}

#[test]
fn nonterminal_delivery_failure_replays_all_pending_entries_before_later_live_publication()
-> Result<()> {
    struct FailFirstAdapter {
        failed: bool,
        events: Vec<PublicRunEvent>,
    }

    impl ApplicationRunEventHandler for FailFirstAdapter {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            if !self.failed {
                self.failed = true;
                anyhow::bail!("adapter is temporarily unavailable");
            }
            self.events.push(event);
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingAdapter(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for RecordingAdapter {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let session = Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&path)?)?;
    let events =
        durable_application_event_sequence(session.session_scope_id(), "run-replay", &path)?;
    let mut failing = FailFirstAdapter {
        failed: false,
        events: Vec::new(),
    };
    events.emit(
        &mut failing,
        PublicRunEventKind::RunStarted {
            prompt: "first".to_owned(),
        },
    )?;
    // The second fact is durable but must not bypass the first pending delivery on the live edge.
    events.emit(
        &mut failing,
        PublicRunEventKind::Notice {
            message: "second".to_owned(),
        },
    )?;
    assert!(events.delivery_is_degraded()?);
    assert!(failing.events.is_empty());

    let mut replay = RecordingAdapter::default();
    let mut bridge = PublicApplicationEventBridge::new(
        durable_application_event_sequence(session.session_scope_id(), "run-replay", &path)?,
        &mut replay,
    )?;
    bridge.emit(PublicRunEventKind::Notice {
        message: "third".to_owned(),
    })?;
    drop(bridge);
    assert_eq!(
        replay
            .0
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3],
        "a fresh bridge replays every durable predecessor before its later live event"
    );

    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &JsonlSessionStore::read_event_records(&path)?,
    )?;
    assert!(projection.pending_for_adapter("application").is_empty());
    Ok(())
}

#[test]
fn conflicting_durable_sequence_does_not_publish_an_unproven_candidate() -> Result<()> {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let session = Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&path)?)?;
    let sequence =
        durable_application_event_sequence(session.session_scope_id(), "run-conflict", &path)?;
    let foreign_event = PublicRunEvent::new(
        session.session_scope_id(),
        "run-conflict",
        1,
        PublicRunEventKind::Notice {
            message: "already durable under another exact identity".to_owned(),
        },
    );
    let foreign_public_event_id = "foreign-public-event".to_owned();
    sigil_kernel::PublicEventOutboxRecorder::new(JsonlSessionStore::new(&path)?).append_outbox(
        &sigil_kernel::PublicEventOutboxEntryV1 {
            schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            domain_event_id: foreign_public_event_id.clone(),
            public_event_id: foreign_public_event_id,
            run_id: "run-conflict".to_owned(),
            sequence: 1,
            payload_digest: sigil_kernel::stable_event_hash(serde_json::to_vec(&foreign_event)?),
            event: foreign_event,
        },
    )?;

    let mut recorder = Recorder::default();
    let error = sequence
        .emit(
            &mut recorder,
            PublicRunEventKind::Notice {
                message: "candidate must never leak".to_owned(),
            },
        )
        .expect_err("same sequence under another identity cannot prove this candidate");
    assert!(is_application_public_outbox_append_error(&error));
    assert_eq!(
        recorder.0.len(),
        1,
        "only the already-durable predecessor is replayed"
    );
    assert!(matches!(
        &recorder.0[0].event,
        PublicRunEventKind::Notice { message }
            if message == "already durable under another exact identity"
    ));
    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &JsonlSessionStore::read_event_records(&path)?,
    )?;
    assert_eq!(projection.events_in_order().len(), 1);
    Ok(())
}

#[tokio::test]
async fn application_execution_does_not_rewrite_an_unconfirmed_public_append_as_run_failed()
-> Result<()> {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "prove the append authority boundary",
        "run-unconfirmed-public-append",
    );
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter));
    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        services,
        &config_path,
        temp.path(),
    )?;
    services.require_current_schema_authority()?;
    let prepared = prepare_application_run(request, &services).await?;
    let session_id = prepared.execution.session_id.clone();
    let run_id = prepared.execution.run_id.clone();
    let session_path = prepared.execution.session_log_path.clone();
    // The execution bridge was initialized at durable sequence zero. A concurrent durable entry
    // at sequence one cannot prove its subsequently prepared RunStarted candidate, even though
    // the adapter can receive/replay the pre-existing entry.
    let foreign_event = PublicRunEvent::new(
        &session_id,
        &run_id,
        1,
        PublicRunEventKind::Notice {
            message: "pre-existing public fact".to_owned(),
        },
    );
    let foreign_public_event_id = "foreign-pre-existing-public-fact".to_owned();
    sigil_kernel::PublicEventOutboxRecorder::new(JsonlSessionStore::new(&session_path)?)
        .append_outbox(&sigil_kernel::PublicEventOutboxEntryV1 {
            schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            domain_event_id: foreign_public_event_id.clone(),
            public_event_id: foreign_public_event_id,
            run_id: run_id.clone(),
            sequence: 1,
            payload_digest: sigil_kernel::stable_event_hash(serde_json::to_vec(&foreign_event)?),
            event: foreign_event,
        })?;

    let mut recorder = Recorder::default();
    let mut approvals = AutoApproveHandler;
    let error = prepared
        .execution
        .execute(&mut recorder, &mut approvals)
        .await
        .expect_err("the runtime must not publish or terminalize an unproven candidate");
    assert!(is_application_public_outbox_append_error(&error));
    assert!(recorder.0.iter().all(|event| {
        !matches!(event.event, PublicRunEventKind::RunFailed { .. })
            && !matches!(event.event, PublicRunEventKind::RunStarted { .. })
    }));
    let records = JsonlSessionStore::read_event_records(&session_path)?;
    assert!(records.iter().all(|record| {
        !matches!(
            record.stored_event().event_kind(),
            Some(sigil_kernel::DurableEventType::RunFinalized)
        )
    }));
    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    assert!(projection.events_in_order().iter().all(|entry| {
        !matches!(entry.event.event, PublicRunEventKind::RunFailed { .. })
            && !matches!(entry.event.event, PublicRunEventKind::RunStarted { .. })
    }));
    Ok(())
}

#[test]
fn failed_terminal_delivery_keeps_the_exact_event_pending_without_rewriting_domain_state()
-> Result<()> {
    struct FailFirstTerminal {
        failed: bool,
        events: Vec<PublicRunEvent>,
    }

    impl ApplicationRunEventHandler for FailFirstTerminal {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            if !self.failed && matches!(event.event, PublicRunEventKind::RunInterrupted { .. }) {
                self.failed = true;
                anyhow::bail!("durable publication failed");
            }
            self.events.push(event);
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let session = Session::load_from_store("deepseek", "model", store)?;
    let lifecycle = session.conversation_run_lifecycle_recorder()?;
    lifecycle.append_started(&ConversationRunStartedEntryV1::new("run-1", 1)?)?;
    let sequence = ApplicationRunEventSequence::with_outbox(
        session.session_scope_id().to_owned(),
        "run-1".to_owned(),
        JsonlSessionStore::new(&path)?,
    )?;
    let mut handler = FailFirstTerminal {
        failed: false,
        events: Vec::new(),
    };
    let terminal = sigil_kernel::ConversationRunFinalizedEntryV1::new(
        "run-1",
        ConversationRunTerminalStatusV1::Interrupted,
        None,
        Some("first terminal"),
        2,
        &sigil_kernel::SecretRedactor::empty(),
    )?;
    sequence.emit_terminal(
        &lifecycle,
        &mut handler,
        &terminal,
        PublicRunEventKind::RunInterrupted {
            reason: "first terminal".to_owned(),
        },
    )?;
    assert_eq!(
        lifecycle
            .finalized_for_run("run-1")?
            .map(|entry| entry.status()),
        Some(ApplicationRunTerminalStatus::Interrupted)
    );
    assert!(sequence.delivery_is_degraded()?);
    let records = JsonlSessionStore::read_event_records(&path)?;
    assert!(records.iter().any(|record| {
        record.stored_event().event_id == "application-domain:run-1:1"
            && record.stored_event().event_kind()
                == Some(sigil_kernel::DurableEventType::RunFinalized)
    }));
    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    assert!(
        projection
            .pending_for_adapter("application")
            .iter()
            .any(|event| {
                event.public_event_id
                    == format!("application-public:{}:run-1:1", session.session_scope_id())
                    && event.domain_event_id == "application-domain:run-1:1"
                    && matches!(event.event.event, PublicRunEventKind::RunInterrupted { .. })
            })
    );
    assert!(
        sequence
            .emit(
                &mut handler,
                PublicRunEventKind::Notice {
                    message: "late".to_owned(),
                },
            )
            .is_err()
    );
    assert!(handler.events.is_empty());
    Ok(())
}

#[test]
fn direct_terminal_control_replays_older_pending_public_events_first() -> Result<()> {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let session = Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&path)?)?;
    let session_id = session.session_scope_id().to_owned();
    let old_event = PublicRunEvent::new(
        &session_id,
        "run-terminal-replay",
        1,
        PublicRunEventKind::Notice {
            message: "older pending progress".to_owned(),
        },
    );
    let old_public_event_id = format!("application-public:{session_id}:run-terminal-replay:1");
    sigil_kernel::PublicEventOutboxRecorder::new(JsonlSessionStore::new(&path)?).append_outbox(
        &sigil_kernel::PublicEventOutboxEntryV1 {
            schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            domain_event_id: old_public_event_id.clone(),
            public_event_id: old_public_event_id,
            run_id: "run-terminal-replay".to_owned(),
            sequence: 1,
            payload_digest: sigil_kernel::stable_event_hash(serde_json::to_vec(&old_event)?),
            event: old_event,
        },
    )?;
    let lifecycle = session.conversation_run_lifecycle_recorder()?;
    lifecycle.append_started(&ConversationRunStartedEntryV1::new(
        "run-terminal-replay",
        1,
    )?)?;
    let sequence = durable_application_event_sequence(&session_id, "run-terminal-replay", &path)?;
    let terminal = sigil_kernel::ConversationRunFinalizedEntryV1::new(
        "run-terminal-replay",
        ConversationRunTerminalStatusV1::Interrupted,
        None,
        Some("stop"),
        2,
        &sigil_kernel::SecretRedactor::empty(),
    )?;
    let mut recorder = Recorder::default();
    sequence.emit_terminal(
        &lifecycle,
        &mut recorder,
        &terminal,
        PublicRunEventKind::RunInterrupted {
            reason: "stop".to_owned(),
        },
    )?;
    assert_eq!(
        recorder
            .0
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &JsonlSessionStore::read_event_records(&path)?,
    )?;
    assert!(projection.pending_for_adapter("application").is_empty());
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn terminal_lifecycle_reuses_the_outbox_sequence_after_terminal_and_replays_lost_receipts()
-> Result<()> {
    #[derive(Default)]
    struct PreviousAdapter;

    impl ApplicationRunEventHandler for PreviousAdapter {
        fn handle_public_event(&mut self, _event: PublicRunEvent) -> Result<()> {
            Ok(())
        }

        fn public_event_adapter_id(&self) -> &'static str {
            "previous_adapter"
        }
    }

    #[derive(Debug)]
    struct LifecycleAdapter {
        session_log_path: std::path::PathBuf,
        parked_session_log_path: std::path::PathBuf,
        lose_first_lifecycle_receipt: AtomicBool,
        events: Arc<Mutex<Vec<PublicRunEvent>>>,
    }

    impl crate::ApplicationTerminalLifecycleHandler for LifecycleAdapter {
        fn handle_public_event(&self, event: PublicRunEvent) -> Result<()> {
            if matches!(&event.event, PublicRunEventKind::TerminalLifecycle { .. })
                && self
                    .lose_first_lifecycle_receipt
                    .swap(false, Ordering::SeqCst)
            {
                std::fs::rename(&self.session_log_path, &self.parked_session_log_path)?;
                std::fs::create_dir(&self.session_log_path)?;
            }
            self.events
                .lock()
                .map_err(|_| anyhow::anyhow!("terminal lifecycle test event state is unavailable"))?
                .push(event);
            Ok(())
        }

        fn public_event_adapter_id(&self) -> &'static str {
            "terminal_lifecycle_test"
        }
    }

    #[derive(Debug)]
    struct RecordingLifecycleAdapter(Arc<Mutex<Vec<PublicRunEvent>>>);

    impl crate::ApplicationTerminalLifecycleHandler for RecordingLifecycleAdapter {
        fn handle_public_event(&self, event: PublicRunEvent) -> Result<()> {
            self.0
                .lock()
                .map_err(|_| anyhow::anyhow!("terminal lifecycle test event state is unavailable"))?
                .push(event);
            Ok(())
        }

        fn public_event_adapter_id(&self) -> &'static str {
            "terminal_lifecycle_test"
        }
    }

    struct RunAdapter(Arc<dyn crate::ApplicationTerminalLifecycleHandler>);

    impl ApplicationRunEventHandler for RunAdapter {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.handle_public_event(event)
        }

        fn public_event_adapter_id(&self) -> &'static str {
            self.0.public_event_adapter_id()
        }
    }

    fn terminal_update(
        generation: u64,
        status: TerminalTaskStatus,
    ) -> Result<TerminalLifecycleUpdateV2> {
        let task = TerminalTaskEntry {
            schema_version: TERMINAL_TASK_SCHEMA_VERSION,
            handle: TerminalTaskHandle {
                task_id: TerminalTaskId::new("terminal-outbox-sequence")?,
                command_sha256: "0".repeat(64),
                cwd_label: ".".to_owned(),
                shell_label: "zsh".to_owned(),
                shell_sha256: "1".repeat(64),
                log_ref: "terminal-log:terminal-outbox-sequence".to_owned(),
                created_at_ms: 1,
                execution_backend: None,
                execution_backend_capabilities: None,
                enforcement_backend: None,
                enforcement_backend_capabilities: None,
                sandbox_profile: None,
            },
            generation,
            status,
            readiness: TerminalReadinessStatus::Ready {
                kind: TerminalReadinessKind::OutputContains,
                ready_at_ms: generation,
            },
            output_preview: None,
            output_hash: None,
            output_truncated: false,
            output_total_bytes: 0,
            output_limit_bytes: None,
            output_termination_reason: None,
            cleanup: None,
            updated_at_ms: generation,
        };
        Ok(TerminalLifecycleUpdateV2 {
            event: TerminalLifecycleEvent {
                task_id: task.handle.task_id.clone(),
                execution_backend: task.handle.execution_backend,
                sandbox_profile: task.handle.sandbox_profile,
                generation: task.generation,
                status: task.status.clone(),
                readiness: task.readiness.clone(),
                total_output_bytes: task.output_total_bytes,
                emitted_at_ms: task.updated_at_ms,
            },
            task,
        })
    }

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let session = Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&path)?)?;
    let session_id = session.session_scope_id().to_owned();
    let lifecycle = session.conversation_run_lifecycle_recorder()?;
    lifecycle.append_started(&ConversationRunStartedEntryV1::new(
        "run-terminal-lifecycle",
        1,
    )?)?;
    let sequence =
        durable_application_event_sequence(&session_id, "run-terminal-lifecycle", &path)?;

    // The public lifecycle adapter must replay this older ordinary event before it can send a
    // lifecycle update, even though another adapter already received it.
    sequence.emit(
        &mut PreviousAdapter,
        PublicRunEventKind::RunStarted {
            prompt: "foreground run".to_owned(),
        },
    )?;
    // A bridge is bound to one active adapter. Reattach from the durable source before the
    // lifecycle adapter takes over; its receipts must not be inferred from the previous adapter.
    let sequence =
        durable_application_event_sequence(&session_id, "run-terminal-lifecycle", &path)?;

    let first_events = Arc::new(Mutex::new(Vec::new()));
    let parked = temp.path().join("session-before-lifecycle-receipt.jsonl");
    let first_handler: Arc<dyn crate::ApplicationTerminalLifecycleHandler> =
        Arc::new(LifecycleAdapter {
            session_log_path: path.clone(),
            parked_session_log_path: parked.clone(),
            lose_first_lifecycle_receipt: AtomicBool::new(true),
            events: Arc::clone(&first_events),
        });
    let router = crate::ApplicationTerminalLifecycleRouter::new(MutationEventRecorder::new(
        JsonlSessionStore::new(&path)?,
    ))
    .with_application_public_events(Arc::clone(&first_handler), sequence.clone());
    sigil_kernel::TerminalLifecycleSink::publish(
        &router,
        terminal_update(1, TerminalTaskStatus::Running)?,
    )
    .await?;
    assert!(sequence.delivery_is_degraded()?);
    assert_eq!(
        first_events
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal lifecycle test event state is unavailable"))?
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );

    std::fs::remove_dir(&path)?;
    std::fs::rename(&parked, &path)?;
    let terminal = sigil_kernel::ConversationRunFinalizedEntryV1::new(
        "run-terminal-lifecycle",
        ConversationRunTerminalStatusV1::Succeeded,
        Some("foreground-final-message".to_owned()),
        Some("foreground complete"),
        2,
        &sigil_kernel::SecretRedactor::empty(),
    )?;
    sequence.emit_terminal(
        &lifecycle,
        &mut RunAdapter(Arc::clone(&first_handler)),
        &terminal,
        PublicRunEventKind::RunFinished {
            final_text: "foreground complete".to_owned(),
        },
    )?;

    let recovered_events = Arc::new(Mutex::new(Vec::new()));
    let recovered_handler: Arc<dyn crate::ApplicationTerminalLifecycleHandler> =
        Arc::new(RecordingLifecycleAdapter(Arc::clone(&recovered_events)));
    let recovered_sequence =
        durable_application_event_sequence(&session_id, "run-terminal-lifecycle", &path)?;
    let mut recovered_run_handler = RunAdapter(Arc::clone(&recovered_handler));
    let late_progress = recovered_sequence.emit(
        &mut recovered_run_handler,
        PublicRunEventKind::Notice {
            message: "invalid late foreground progress".to_owned(),
        },
    );
    assert!(
        late_progress
            .expect_err("a reopened finalized run must still reject ordinary progress")
            .to_string()
            .contains("already terminal")
    );
    let recovered_router = crate::ApplicationTerminalLifecycleRouter::new(
        MutationEventRecorder::new(JsonlSessionStore::new(&path)?),
    )
    .with_application_public_events(recovered_handler, recovered_sequence);
    sigil_kernel::TerminalLifecycleSink::publish(
        &recovered_router,
        terminal_update(2, TerminalTaskStatus::Exited { exit_code: Some(0) })?,
    )
    .await?;

    assert_eq!(
        recovered_events
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal lifecycle test event state is unavailable"))?
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3, 4],
        "receipt recovery must replay lifecycle and foreground terminal before late exit"
    );
    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &JsonlSessionStore::read_event_records(&path)?,
    )?;
    let outbox = projection
        .events_in_order()
        .into_iter()
        .filter(|entry| entry.run_id == "run-terminal-lifecycle")
        .collect::<Vec<_>>();
    assert_eq!(
        outbox
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert!(matches!(
        &outbox[0].event.event,
        PublicRunEventKind::RunStarted { .. }
    ));
    assert!(matches!(
        &outbox[1].event.event,
        PublicRunEventKind::TerminalLifecycle { .. }
    ));
    assert!(matches!(
        &outbox[2].event.event,
        PublicRunEventKind::RunFinished { .. }
    ));
    assert!(matches!(
        &outbox[3].event.event,
        PublicRunEventKind::TerminalLifecycle { .. }
    ));
    assert!(
        projection
            .pending_for_adapter("terminal_lifecycle_test")
            .is_empty()
    );
    Ok(())
}

#[test]
fn durable_outbox_replays_a_terminal_with_its_original_public_event_id() -> Result<()> {
    struct FailingAdapter;

    impl ApplicationRunEventHandler for FailingAdapter {
        fn handle_public_event(&mut self, _event: PublicRunEvent) -> Result<()> {
            anyhow::bail!("adapter is temporarily unavailable")
        }
    }

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let session = Session::load_from_store("deepseek", "model", store)?;
    let lifecycle = session.conversation_run_lifecycle_recorder()?;
    lifecycle.append_started(&ConversationRunStartedEntryV1::new("run-outbox", 1)?)?;
    let sequence = ApplicationRunEventSequence::with_outbox(
        session.session_scope_id().to_owned(),
        "run-outbox".to_owned(),
        JsonlSessionStore::new(&path)?,
    )?;
    let mut adapter = FailingAdapter;
    let terminal = sigil_kernel::ConversationRunFinalizedEntryV1::new(
        "run-outbox",
        ConversationRunTerminalStatusV1::Blocked,
        None,
        Some("provider route needs recovery"),
        2,
        &sigil_kernel::SecretRedactor::empty(),
    )?;
    sequence.emit_terminal(
        &lifecycle,
        &mut adapter,
        &terminal,
        PublicRunEventKind::RunBlocked {
            reason: "provider route needs recovery".to_owned(),
        },
    )?;

    assert!(sequence.delivery_is_degraded()?);
    assert!(!sequence.terminal_was_delivered()?);
    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &JsonlSessionStore::read_event_records(&path)?,
    )?;
    let pending = projection.pending_for_adapter("application");
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].public_event_id,
        format!(
            "application-public:{}:run-outbox:1",
            session.session_scope_id()
        )
    );
    // A fresh control has no emission acknowledgement in memory. Its answer still comes from
    // the original paired domain terminal, including after an adapter failed to accept it.
    let control = ApplicationRunControl {
        owner: RunCancellationOwner::new(),
        recorder: session.run_cancellation_recorder()?,
        cancellation_target: RunCancellationTarget::Run,
        conversation_lifecycle: lifecycle.clone(),
        conversation_start: ConversationRunStartedEntryV1::new("run-outbox", 1)?,
        events: durable_application_event_sequence(
            session.session_scope_id(),
            "run-outbox",
            &path,
        )?,
        _session_lease: Arc::new(ApplicationSessionLeaseManager::new().acquire(&path)?),
    };
    assert!(!control.terminal_was_delivered()?);
    assert_eq!(
        control.durable_terminal_status()?,
        Some(ApplicationRunTerminalStatus::Blocked)
    );
    assert!(matches!(
        pending[0].event.event,
        PublicRunEventKind::RunBlocked { .. }
    ));
    struct RecordingAdapter {
        events: Vec<PublicRunEvent>,
    }

    impl ApplicationRunEventHandler for RecordingAdapter {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.events.push(event);
            Ok(())
        }
    }

    let mut replay = RecordingAdapter { events: Vec::new() };
    assert_eq!(
        replay_pending_application_outbox(&path, "run-outbox", &mut replay)?,
        1
    );
    assert_eq!(replay.events.len(), 1);
    assert_eq!(replay.events[0].sequence, 1);
    assert!(matches!(
        replay.events[0].event,
        PublicRunEventKind::RunBlocked { .. }
    ));
    assert_eq!(
        replay_pending_application_outbox(&path, "run-outbox", &mut replay)?,
        0
    );
    Ok(())
}

#[test]
fn receipt_write_failure_keeps_terminal_outbox_pending_for_exact_second_replay() -> Result<()> {
    struct FailingAdapter;

    impl ApplicationRunEventHandler for FailingAdapter {
        fn handle_public_event(&mut self, _event: PublicRunEvent) -> Result<()> {
            anyhow::bail!("adapter is temporarily unavailable")
        }
    }

    struct ReceiptWriteFailingAdapter {
        session_log_path: std::path::PathBuf,
        parked_session_log_path: std::path::PathBuf,
        event: Option<PublicRunEvent>,
    }

    impl ApplicationRunEventHandler for ReceiptWriteFailingAdapter {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            std::fs::rename(&self.session_log_path, &self.parked_session_log_path)?;
            std::fs::create_dir(&self.session_log_path)?;
            self.event = Some(event);
            Ok(())
        }
    }

    struct RecordingAdapter {
        events: Vec<PublicRunEvent>,
    }

    impl ApplicationRunEventHandler for RecordingAdapter {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.events.push(event);
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    {
        let store = JsonlSessionStore::new(&path)?;
        let session = Session::load_from_store("deepseek", "model", store)?;
        let lifecycle = session.conversation_run_lifecycle_recorder()?;
        lifecycle.append_started(&ConversationRunStartedEntryV1::new("run-receipt", 1)?)?;
        let sequence = ApplicationRunEventSequence::with_outbox(
            session.session_scope_id().to_owned(),
            "run-receipt".to_owned(),
            JsonlSessionStore::new(&path)?,
        )?;
        let terminal = sigil_kernel::ConversationRunFinalizedEntryV1::new(
            "run-receipt",
            ConversationRunTerminalStatusV1::Paused,
            None,
            Some("operator must resume the task"),
            2,
            &sigil_kernel::SecretRedactor::empty(),
        )?;
        let mut adapter = FailingAdapter;
        sequence.emit_terminal(
            &lifecycle,
            &mut adapter,
            &terminal,
            PublicRunEventKind::RunPaused {
                reason: "operator must resume the task".to_owned(),
            },
        )?;
    }

    let parked = temp.path().join("session-before-receipt.jsonl");
    let mut receipt_failure = ReceiptWriteFailingAdapter {
        session_log_path: path.clone(),
        parked_session_log_path: parked.clone(),
        event: None,
    };
    assert!(replay_pending_application_outbox(&path, "run-receipt", &mut receipt_failure).is_err());
    let first_delivery = receipt_failure
        .event
        .take()
        .expect("adapter should have accepted the event before its receipt write failed");

    std::fs::remove_dir(&path)?;
    std::fs::rename(&parked, &path)?;

    let mut replay = RecordingAdapter { events: Vec::new() };
    assert_eq!(
        replay_pending_application_outbox(&path, "run-receipt", &mut replay)?,
        1
    );
    assert_eq!(
        serde_json::to_value(&replay.events)?,
        serde_json::to_value(vec![first_delivery])?
    );
    assert_eq!(
        replay_pending_application_outbox(&path, "run-receipt", &mut replay)?,
        0
    );
    Ok(())
}

#[test]
fn failed_task_preserves_durable_error_instead_of_delegation_blocker() -> Result<()> {
    let task_id = TaskId::new("task-terminal-failure")?;
    let reason = "missing terminal task field schema_version";
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        objective: "read command output".to_owned(),
        title: None,
        status: TaskRunStatus::Failed,
        reason: Some(reason.to_owned()),
    }))?;
    let output = AgentRunOutput {
        disposition: AgentRunDisposition::FinalAnswer,
        result: AgentRunResult {
            final_text: "previous turn".to_owned(),
            tool_calls: 1,
            final_message_id: None,
        },
        outcome: AgentRunOutcome::default(),
    };

    let error = application_task_terminal_output(&session, &task_id, TaskRunStatus::Failed, output)
        .expect_err("failed task must enter the application failure finalizer");
    assert!(error.downcast_ref::<ApplicationTaskFailed>().is_some());
    assert_eq!(
        error.to_string(),
        format!("task {} failed: {reason}", task_id.as_str())
    );
    Ok(())
}

#[test]
fn unfinished_task_does_not_claim_delegation_failure() -> Result<()> {
    let task_id = TaskId::new("task-terminal-pending")?;
    for task_status in [
        TaskRunStatus::Started,
        TaskRunStatus::Running,
        TaskRunStatus::Paused,
    ] {
        let output = application_task_terminal_output(
            &Session::new("deepseek", "deepseek-v4-flash"),
            &task_id,
            task_status,
            AgentRunOutput {
                disposition: AgentRunDisposition::FinalAnswer,
                result: AgentRunResult {
                    final_text: String::new(),
                    tool_calls: 0,
                    final_message_id: None,
                },
                outcome: AgentRunOutcome::default(),
            },
        )?;
        assert_eq!(
            output.outcome.terminal_reason,
            AgentRunTerminalReason::TaskHandoff
        );
        assert!(matches!(application_terminal_projection(&output), (
            ApplicationRunTerminalStatus::Blocked,
            PublicRunEventKind::RunBlocked { reason }
        ) if reason == "run is waiting for its durable task to complete"));
    }
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
        assert!(matches!(
            (terminal_reason, event),
            (
                AgentRunTerminalReason::MaxTurns,
                PublicRunEventKind::RunInterrupted { .. }
            ) | (
                AgentRunTerminalReason::DelegationUnsatisfied,
                PublicRunEventKind::RunBlocked { .. }
            )
        ));
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
    assert!(matches!(event, PublicRunEventKind::RunBlocked { .. }));
    Ok(())
}

#[tokio::test]
async fn application_manual_run_does_not_prepare_plan_review() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let mut config = RootConfig::load(&config_path)?;
    config.task.enabled = false;
    config.task.routing_policy = TaskRoutingPolicy::Manual;
    config.save(&config_path)?;
    let request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "read the source file",
        "run-manual-core",
    );
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter));
    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        services,
        &config_path,
        temp.path(),
    )?;
    services.require_current_schema_authority()?;
    let prepared = prepare_application_run(request, &services).await?;
    assert!(matches!(
        prepared.execution.kind,
        ApplicationRunExecutionKind::Main { .. }
    ));
    assert!(prepared.execution.plan_review_runtime.is_none());
    assert!(prepared.execution.task_execution.is_none());
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn application_auto_routing_stays_manual_without_attached_task_executor() -> Result<()> {
    let _environment_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-api-key");
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let storage = isolated_storage_toml(&config_path);
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

{storage}

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
credential = {{ source = "environment", name = "SIGIL_API_KEY" }}
"#
        ),
    )?;
    let request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "implement the cross-layer feature",
        "run-application-without-task-executor",
    );
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter));

    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        services,
        &config_path,
        temp.path(),
    )?;
    services.require_current_schema_authority()?;
    let prepared = prepare_application_run(request, &services).await?;

    assert!(!services.task_executor_attached());
    assert!(prepared.execution.task_execution.is_none());
    assert!(prepared.execution.plan_review_runtime.is_some());
    let ApplicationRunExecutionKind::Main { input, .. } = &prepared.execution.kind else {
        panic!("ordinary application request must prepare the main agent");
    };
    let Some(AgentRunPurpose::Conversation(context)) = input.purpose.as_ref() else {
        panic!("ordinary application request must carry conversation purpose");
    };
    // Without an attached task executor the route stays at the ReviewFirst baseline: plan
    // review remains usable, but the direct task decision is never exposed.
    assert_eq!(context.routing_policy, TaskRoutingPolicy::Auto);
    assert_eq!(
        context.route_capability,
        sigil_kernel::AutomaticRouteCapability::ReviewFirst
    );
    assert!(context.task_handoff.is_none());
    assert!(context.plan_review.is_some());
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn application_preparation_enables_auto_handoff_without_rollout_manifest_or_host_classification()
-> Result<()> {
    // Attaching the task-role provider constructs the configured DeepSeek route during
    // preparation. Keep that credential lookup hermetic instead of inheriting a developer key or
    // racing another test's process-global environment mutation.
    let _environment_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-api-key");
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let storage = isolated_storage_toml(&config_path);
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

{storage}

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
credential = {{ source = "environment", name = "SIGIL_API_KEY" }}
"#
        ),
    )?;
    let request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "implement the cross-layer feature",
        "run-application-auto-handoff",
    );
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter))
        .with_task_role_provider_builder(Arc::new(ApplicationTaskRoleProviderBuilder));

    let _rollout_guard = crate::test_env::EnvScope::set(
        "SIGIL_ORCHESTRATION_ROLLOUT_MANIFEST",
        temp.path().join("missing-rollout.json").as_os_str(),
    );
    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        services,
        &config_path,
        temp.path(),
    )?;
    services.require_current_schema_authority()?;
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
    let storage = isolated_storage_toml(&config_path);
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

{storage}

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
credential = {{ source = "none" }}
"#
        ),
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
        title: None,

        status: TaskRunStatus::Started,
        reason: Some("accepted by the application conversation coordinator".to_owned()),
    }))?;
    session.append_control(ControlEntry::TaskDirectExecutionAdmittedV1(
        TaskDirectExecutionAdmittedV1::task_request(
            task_id.clone(),
            "inspect the application runtime",
            1,
        ),
    ))?;
    let profile_registry =
        crate::AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &root_config,
            temp.path(),
            session.entries(),
        )?;
    let task_execution = ApplicationTaskExecutionRuntime {
        root_config: root_config.clone(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        options: crate::build_run_options(
            &root_config,
            temp.path().to_path_buf(),
            InteractionMode::Headless,
            None,
        ),
        base_registry: {
            let mut registry = ToolRegistry::new();
            registry.register(Arc::new(NamedTool("read_file")));
            registry
        },
        agent_supervisor: crate::AgentSupervisor::new(
            profile_registry,
            crate::AgentBudgetPolicy::from_root_config(&root_config),
            application_task_provider_capabilities(),
        ),
        role_provider_builder: Arc::new(ApplicationTaskRoleProviderBuilder),
        verification_execution_port: Some(Arc::new(LocalExecutionBackend)),
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

    let output = Box::pin(continue_application_task_handoff(
        &mut session,
        root_output,
        Some(task_execution),
        &mut handler,
        &mut approval_handler,
        &cancellation_handle,
    ))
    .await?;

    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert_eq!(output.result.final_text, "application task step completed");
    assert!(output.result.final_message_id.is_some());
    assert!(cancellation_handle.is_naturally_finalized());
    let projection = session.task_state_projection();
    let task = projection.tasks.get(&task_id).expect("task should exist");
    assert_eq!(task.status, TaskRunStatus::Completed);
    assert!(task.direct_execution_attempts.values().any(|attempt| {
        attempt.status == sigil_kernel::TaskExecutionAttemptStatus::Completed
            && attempt.final_message_id.as_deref() == output.result.final_message_id.as_deref()
    }));
    Ok(())
}

#[tokio::test]
async fn application_task_continuation_reopens_exact_task_and_returns_synthesis() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let services = with_application_test_managed_authority(
        temp.path(),
        ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter)),
    )?
    .with_task_role_provider_builder(Arc::new(ApplicationTaskRoleProviderBuilder));
    let binding = bind_application_test_managed_session(
        &config_path,
        temp.path(),
        &temp.path().join("session.jsonl"),
        &services,
    )?;
    let session_path = binding.session_log_path;
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
    crate::bind_session_composition(&mut session, &root_config)?;
    let task_id = TaskId::new("task-application-continuation")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative(
            session_path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("managed session leaf"),
        )?,
        objective: "continue the application task".to_owned(),
        title: None,
        status: TaskRunStatus::Paused,
        reason: Some("application restart".to_owned()),
    }))?;
    session.append_control(ControlEntry::TaskDirectExecutionAdmittedV1(
        TaskDirectExecutionAdmittedV1::task_request(
            task_id.clone(),
            "continue the application task",
            1,
        ),
    ))?;
    let unrelated_user = ModelMessage::user("explain an unrelated module first");
    let unrelated_user_id = unrelated_user.id.clone();
    session.append_user_message(unrelated_user)?;
    assert!(
        session.task_state_projection().current_task().is_none(),
        "an ordinary User turn must clear the previous Task focus before explicit continuation"
    );
    let session_scope_id = session.session_scope_id().to_owned();
    drop(session);
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
        Some("application task step completed")
    );
    assert!(matches!(
        events.0.first().map(|event| &event.event),
        Some(PublicRunEventKind::RunStarted { .. })
    ));
    assert!(matches!(
        events.0.last().map(|event| &event.event),
        Some(PublicRunEventKind::RunFinished { final_text })
            if final_text == "application task step completed"
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
    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::User(_)))
            .count(),
        1,
        "Task continuation must not synthesize another user conversation prompt"
    );
    assert!(reopened.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::User(message) if message.id == unrelated_user_id
    )));
    assert_eq!(
        reopened
            .task_state_projection()
            .tasks
            .get(&task_id)
            .map(|task| task.status),
        Some(TaskRunStatus::Completed)
    );
    assert!(reopened.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskDirectExecutionAttemptV1(attempt))
            if attempt.task_id == task_id
                && attempt.status == sigil_kernel::TaskExecutionAttemptStatus::Completed
    )));
    Ok(())
}

#[tokio::test]
async fn application_task_continuation_rejects_stale_scope_without_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let storage = isolated_storage_toml(&config_path);
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

{storage}

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
credential = {{ source = "environment", name = "SIGIL_API_KEY" }}
"#
        ),
    )?;
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    let task_id = TaskId::new("task-stale-application-continuation")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        objective: "reject stale continuation".to_owned(),
        title: None,

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
        events: durable_application_event_sequence(
            session.session_scope_id(),
            "run-task-pause",
            &store_path,
        )?,
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
    let pause_request = TaskPauseRequest::direct(
        task_id.clone(),
        TaskDirectExecutionAdmittedV1::task_request(
            task_id.clone(),
            "control an application Task",
            1,
        )
        .admission_id,
    );
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
    assert!(
        events
            .0
            .iter()
            .any(|event| { matches!(event.event, PublicRunEventKind::RunPaused { .. }) })
    );
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
        ] if finalized.status() == ConversationRunTerminalStatusV1::Paused
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
        events: durable_application_event_sequence(
            session.session_scope_id(),
            "run-task-stale-pause",
            &store_path,
        )?,
        _session_lease: Arc::new(ApplicationSessionLeaseManager::new().acquire(&store_path)?),
    };

    let error = control
        .request_task_pause(
            TaskPauseRequest::direct(task_id, "stale-admission"),
            None,
            || {},
        )
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
    let pause_request = TaskPauseRequest::direct(
        task_id.clone(),
        TaskDirectExecutionAdmittedV1::task_request(
            task_id.clone(),
            "control an application Task",
            1,
        )
        .admission_id,
    );
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
        events: durable_application_event_sequence(
            session.session_scope_id(),
            "run-task-unaudited-pause",
            &store_path,
        )?,
        _session_lease: Arc::new(ApplicationSessionLeaseManager::new().acquire(&store_path)?),
    };
    let ticket = ApplicationTaskPauseTicket {
        request: pause_request,
        cancellation: ApplicationCancellationTicket {
            request: cancellation_request,
            started: std::time::Instant::now(),
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
    assert!(
        events
            .0
            .iter()
            .any(|event| { matches!(event.event, PublicRunEventKind::RunInterrupted { .. }) })
    );
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
        events: durable_application_event_sequence(
            session.session_scope_id(),
            "run-task-pause-interrupted",
            &store_path,
        )?,
        _session_lease: Arc::new(ApplicationSessionLeaseManager::new().acquire(&store_path)?),
    };
    let ticket = control.request_task_pause(
        TaskPauseRequest::direct(
            task_id.clone(),
            TaskDirectExecutionAdmittedV1::task_request(
                task_id.clone(),
                "control an application Task",
                1,
            )
            .admission_id,
        ),
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
    assert!(
        events
            .0
            .iter()
            .any(|event| { matches!(event.event, PublicRunEventKind::RunInterrupted { .. }) })
    );
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
        events: durable_application_event_sequence(
            session.session_scope_id(),
            "run-1",
            &store_path,
        )?,
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
        events: durable_application_event_sequence(
            session.session_scope_id(),
            "run-task-cancel",
            &store_path,
        )?,
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
        events: durable_application_event_sequence(
            session.session_scope_id(),
            "run-chat-cancel",
            &store_path,
        )?,
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
        events: durable_application_event_sequence(
            session.session_scope_id(),
            "run-1",
            &store_path,
        )?,
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
    assert!(
        events
            .0
            .iter()
            .any(|event| { matches!(event.event, PublicRunEventKind::RunInterrupted { .. }) })
    );
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
async fn cancellation_audit_failure_still_unblocks_without_inventing_a_public_terminal()
-> Result<()> {
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
        events: durable_application_event_sequence(
            session.session_scope_id(),
            "run-1",
            &store_path,
        )?,
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
    assert!(
        events.0.is_empty(),
        "no durable start or terminal was written, so no terminal may be published"
    );
    assert!(control.durable_terminal_status().is_err());
    Ok(())
}

struct PlanReviewDraftProvider;

#[async_trait]
impl Provider for PlanReviewDraftProvider {
    fn name(&self) -> &str {
        "plan-review-draft"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        application_task_provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        if request
            .tools
            .iter()
            .any(|tool| tool.name == sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME)
        {
            let args = r##"{
                "schema_version": 1,
                "outcome": "draft",
                "content": "# Migrate the coordinator\n\n1. Migrate coordinator."
            }"##;
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "plan-draft-call".to_owned(),
                    name: sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "plan-draft-call".to_owned(),
                    delta: args.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "plan-draft-call".to_owned(),
                    name: sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned(),
                    args_json: args.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("plan review ready".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct PlanReviewCandidateProvider;

#[async_trait]
impl Provider for PlanReviewCandidateProvider {
    fn name(&self) -> &str {
        "plan-review-finalizing-draft"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        application_task_provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let _ = request;
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(
                "research completed without a draft".to_owned(),
            )),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn application_plan_review_continuation_commits_typed_draft_and_waits_for_decision()
-> Result<()> {
    // The DeepSeek provider construction requires the credential environment variable; pin a
    // placeholder so the targeted test is hermetic and does not depend on the developer machine.
    // The global lock serializes environment mutation against other tests reading credentials.
    let _environment_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-api-key");
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let storage = isolated_storage_toml(&config_path);
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

{storage}

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
credential = {{ source = "environment", name = "SIGIL_API_KEY" }}
"#
        ),
    )?;
    let root_config: RootConfig = toml::from_str(&std::fs::read_to_string(&config_path)?)?;
    let _rollout_guard =
        crate::tests::rollout_manifest_test_support::qualified_rollout_manifest_guard(&root_config);
    let request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "design the coordinator migration",
        "run-application-plan-review",
    );
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter));
    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        services,
        &config_path,
        temp.path(),
    )?;
    services.require_current_schema_authority()?;
    let prepared = prepare_application_run(request, &services).await?;
    let ApplicationRunExecutionKind::Main { input, .. } = &prepared.execution.kind else {
        panic!("ordinary application request must prepare the main agent");
    };
    let Some(AgentRunPurpose::Conversation(context)) = input.purpose.as_ref() else {
        panic!("conversation purpose expected");
    };
    let plan_review_binding = context.plan_review.clone().expect("plan review binding");
    let source_turn = context.source_turn.clone();
    assert!(
        sigil_kernel::PlanReviewProjection::from_entries(prepared.execution.session.entries())
            .reviews()
            .next()
            .is_none(),
        "automatic preparation must leave its first attempt for the executor's public-control bundle"
    );
    drop(prepared);

    let session_path = temp.path().join("plan-review-parent.jsonl");
    let mut session = Session::load_from_store(
        "application-plan-review",
        "planned-model",
        JsonlSessionStore::new(&session_path)?,
    )?;
    // This routing outcome is simulated in a separate parent from the preparation fixture.
    // Bind the real source message and its decision to that parent's durable identity.
    let source_turn = sigil_kernel::ConversationTurnRef::new(
        session.session_scope_id(),
        source_turn.message_id,
        source_turn.logical_run_id,
    )?;
    // Simulate the routing microturn outcome: the model requests a plan review.
    let agent_output = AgentRunOutput {
        result: sigil_kernel::AgentRunResult {
            final_text: String::new(),
            tool_calls: 1,
            final_message_id: None,
        },
        outcome: sigil_kernel::AgentRunOutcome::default(),
        disposition: AgentRunDisposition::StartPlanReview(StartPlanReviewAction {
            decision_id: plan_review_binding.decision_id.clone(),
            plan_review_id: plan_review_binding.plan_review_id.clone(),
            plan_id: plan_review_binding.plan_id.clone(),
            source_turn: source_turn.clone(),
        }),
    };
    start_application_public_control_run(&session, "run-application-plan-review")?;
    let mut message = ModelMessage::user("design the coordinator migration");
    message.id = source_turn.message_id.clone();
    session.append_user_message(message)?;
    session.append_control(ControlEntry::ConversationRouteDecisionRecorded(
        sigil_kernel::ConversationRouteDecisionRecordedEntry {
            decision_id: plan_review_binding.decision_id.clone(),
            source_turn,
            route: sigil_kernel::ConversationRoute::PlanReview,
            reason_codes: vec![sigil_kernel::ConversationRouteReason::ScopeUncertain],
            configured_policy: TaskRoutingPolicy::Auto,
            effective_capability: sigil_kernel::AutomaticRouteCapability::ReviewFirst,
            policy_snapshot_hash: plan_review_binding.policy_snapshot_hash.clone(),
            route_contract_fingerprint: plan_review_binding.route_contract_fingerprint.clone(),
            decided_at_ms: 42,
        },
    ))?;
    let options = crate::build_run_options(
        &root_config,
        temp.path().to_path_buf(),
        sigil_kernel::InteractionMode::Headless,
        None,
    );
    let runtime = super::ApplicationPlanReviewRuntime {
        options,

        agent: Box::new(sigil_kernel::Agent::new(
            Box::new(PlanReviewDraftProvider),
            sigil_kernel::ToolRegistry::new(),
        )),
        tool_registry: sigil_kernel::ToolRegistry::new(),
        workspace_snapshot_id: None,
        child_resource_provisioner: None,
    };
    let cancellation_owner = sigil_kernel::RunCancellationOwner::new();
    let mut recorder = RecordingApplicationRunEvents::default();
    let events = durable_application_event_sequence(
        session.session_scope_id(),
        "run-application-plan-review",
        &session_path,
    )?;
    let mut handler = PublicApplicationEventBridge::new(events, &mut recorder)?;
    let mut approval_handler = sigil_kernel::AutoApproveHandler;
    let output = super::continue_application_plan_review(
        &mut session,
        agent_output,
        Some(runtime),
        &mut handler,
        &mut approval_handler,
        &cancellation_owner.handle(),
        "run-application-plan-review",
        &sigil_kernel::SecretRedactor::empty(),
    )
    .await?;
    assert!(matches!(
        output.disposition,
        AgentRunDisposition::FinalAnswer
    ));
    assert!(output.result.final_text.contains("Plan ready"));
    let final_message_id = output
        .result
        .final_message_id
        .expect("plan-ready success must bind a durable final assistant");
    drop(handler);
    assert!(recorder.0.iter().any(|event| matches!(
        &event.event,
        PublicRunEventKind::PlanReviewChanged { status, .. }
            if *status == sigil_kernel::PublicPlanReviewStatus::DraftReady
    )));
    let plan_projection = session.plan_artifact_projection();
    let draft = plan_projection
        .plans
        .get(&plan_review_binding.plan_id)
        .expect("draft committed to parent");
    assert_eq!(draft.summary, "Migrate the coordinator");
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Assistant(message)
            if message.id == final_message_id
                && message.assistant_kind == Some(AssistantMessageKind::FinalAnswer)
    )));
    let review_projection = sigil_kernel::PlanReviewProjection::from_entries(session.entries());
    let attempt = review_projection
        .latest_attempt(&plan_review_binding.plan_review_id)
        .expect("attempt");
    assert_eq!(
        attempt.status,
        sigil_kernel::PlanReviewAttemptStatus::DraftReady
    );

    let records = JsonlSessionStore::read_event_records(&session_path)?;
    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    let plan_changes = outbox
        .events_in_order()
        .into_iter()
        .filter(|entry| {
            matches!(
                &entry.event.event,
                PublicRunEventKind::PlanReviewChanged { plan_review_id, .. }
                    if plan_review_id == plan_review_binding.plan_review_id.as_str()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(plan_changes.len(), 2);
    assert!(matches!(
        &plan_changes[0].event.event,
        PublicRunEventKind::PlanReviewChanged { status, .. }
            if *status == sigil_kernel::PublicPlanReviewStatus::Started
    ));
    assert!(matches!(
        &plan_changes[1].event.event,
        PublicRunEventKind::PlanReviewChanged { status, .. }
            if *status == sigil_kernel::PublicPlanReviewStatus::DraftReady
    ));
    assert!(plan_changes[0].sequence < plan_changes[1].sequence);
    let all_sequences = outbox
        .events_in_order()
        .into_iter()
        .filter(|entry| entry.run_id == "run-application-plan-review")
        .map(|entry| entry.sequence)
        .collect::<Vec<_>>();
    assert!(
        all_sequences
            .windows(2)
            .all(|window| window[1] == window[0] + 1),
        "the complete run outbox, not merely its PlanReviewChanged subset, stays contiguous"
    );

    let mut attempt_sources = Vec::new();
    let mut draft_index = None;
    for (index, record) in records.iter().enumerate() {
        match record.session_log_entry()? {
            Some(SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft)))
                if draft.plan_id == plan_review_binding.plan_id =>
            {
                draft_index = Some(index);
            }
            Some(SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt)))
                if attempt.plan_review_id == plan_review_binding.plan_review_id =>
            {
                attempt_sources.push((
                    index,
                    attempt.status,
                    record.stored_event().event_id.to_string(),
                ));
            }
            _ => {}
        }
    }
    assert_eq!(
        attempt_sources
            .iter()
            .map(|(_, status, _)| *status)
            .collect::<Vec<_>>(),
        vec![
            sigil_kernel::PlanReviewAttemptStatus::Started,
            sigil_kernel::PlanReviewAttemptStatus::DraftReady,
        ],
        "the executor must append Started exactly once, then the completed attempt"
    );
    let draft_ready_source = attempt_sources
        .iter()
        .find(|(_, status, _)| *status == sigil_kernel::PlanReviewAttemptStatus::DraftReady)
        .expect("draft-ready attempt source");
    assert_eq!(
        draft_index.expect("draft source") + 1,
        draft_ready_source.0,
        "the original draft plus attempt batch must remain intact"
    );
    for (source_index, _, domain_event_id) in &attempt_sources {
        let paired = plan_changes
            .iter()
            .find(|entry| entry.domain_event_id == *domain_event_id)
            .expect("each ordinary Attempt source has its exact PlanReviewChanged outbox entry");
        let public_record = records
            .get(source_index + 1)
            .expect("outbox entry follows its source in the same append bundle")
            .stored_event();
        assert_eq!(
            public_record.event_id.to_string(),
            paired.public_event_id,
            "public outbox identity must be committed beside its domain source"
        );
        assert_eq!(
            public_record
                .causation_id
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some(domain_event_id.as_str())
        );
    }
    let (terminal_index, terminal_domain_id) = records
        .iter()
        .enumerate()
        .find_map(|(index, record)| {
            matches!(
                conversation_run_lifecycle_record_from_stream(record),
                Ok(Some(ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(ref entry)))
                    if entry.run_id() == "run-application-plan-review"
            )
            .then(|| (index, record.stored_event().event_id.to_string()))
        })
        .expect("ordinary Plan review must finalize its enclosing conversation run");
    let terminal_outbox = outbox
        .events_in_order()
        .into_iter()
        .find(|entry| {
            entry.domain_event_id == terminal_domain_id
                && matches!(entry.event.event, PublicRunEventKind::RunFinished { .. })
        })
        .expect("conversation terminal must have an exact RunFinished outbox entry");
    let terminal_public_index = records
        .iter()
        .position(|record| record.stored_event().event_id == terminal_outbox.public_event_id)
        .expect("terminal public outbox record must be durable");
    assert_eq!(terminal_public_index, terminal_index + 1);
    let final_answer_index = records
        .iter()
        .enumerate()
        .find_map(|(index, record)| {
            matches!(
                record.session_log_entry(),
                Ok(Some(SessionLogEntry::Assistant(message)))
                    if message.id == final_message_id
            )
            .then_some(index)
        })
        .expect("final answer source must be durable");
    assert!(draft_ready_source.0 < final_answer_index);
    assert!(final_answer_index < terminal_index);
    Ok(())
}

#[tokio::test]
async fn explicit_application_plan_review_starts_once_and_projects_its_parent_controls()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let root_config = RootConfig::load(&config_path)?;
    let plan = crate::application_extension_catalog_view(&root_config, temp.path(), &[])?
        .agents
        .into_iter()
        .find(|agent| agent.id == "plan" && agent.available)
        .expect("the built-in plan agent should be available");
    let mut request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "prepare a typed application plan",
        "run-explicit-plan-public-controls",
    );
    request.agent_binding = plan.binding;
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter));
    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        services,
        &config_path,
        temp.path(),
    )?;
    services.require_current_schema_authority()?;
    let mut prepared = prepare_application_run(request, &services).await?;
    let plan_review_request = match &prepared.execution.kind {
        ApplicationRunExecutionKind::ExplicitPlanReview { request } => (**request).clone(),
        _ => panic!("built-in plan agent must prepare an explicit review"),
    };
    assert!(
        sigil_kernel::PlanReviewProjection::from_entries(prepared.execution.session.entries())
            .reviews()
            .next()
            .is_none(),
        "prepare must not create an executor-owned Started attempt"
    );
    let runtime = prepared
        .execution
        .plan_review_runtime
        .as_mut()
        .expect("explicit execution retains its plan-review runtime");
    *runtime.agent =
        sigil_kernel::Agent::new(Box::new(PlanReviewDraftProvider), ToolRegistry::new());
    runtime.tool_registry = ToolRegistry::new();
    let session_path = prepared.execution.session_log_path.clone();
    let mut recorder = RecordingApplicationRunEvents::default();
    let output = prepared
        .execution
        .execute(&mut recorder, &mut AutoApproveHandler)
        .await?;
    assert_eq!(
        output.terminal_status,
        ApplicationRunTerminalStatus::Succeeded
    );

    let records = JsonlSessionStore::read_event_records(&session_path)?;
    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    let plan_changes = outbox
        .events_in_order()
        .into_iter()
        .filter(|entry| {
            matches!(
                &entry.event.event,
                PublicRunEventKind::PlanReviewChanged { plan_review_id, .. }
                    if plan_review_id == plan_review_request.plan_review_id.as_str()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(plan_changes.len(), 2);
    assert!(plan_changes[0].sequence < plan_changes[1].sequence);
    let all_sequences = outbox
        .events_in_order()
        .into_iter()
        .filter(|entry| entry.run_id == output.run_id)
        .map(|entry| entry.sequence)
        .collect::<Vec<_>>();
    assert!(
        all_sequences
            .windows(2)
            .all(|window| window[1] == window[0] + 1),
        "the complete run outbox, not merely its PlanReviewChanged subset, stays contiguous"
    );

    let mut attempt_sources = Vec::new();
    let mut draft_index = None;
    for (index, record) in records.iter().enumerate() {
        match record.session_log_entry()? {
            Some(SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft)))
                if draft.plan_id == plan_review_request.plan_id =>
            {
                draft_index = Some(index);
            }
            Some(SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt)))
                if attempt.plan_review_id == plan_review_request.plan_review_id =>
            {
                attempt_sources.push((
                    index,
                    attempt.status,
                    record.stored_event().event_id.to_string(),
                ));
            }
            _ => {}
        }
    }
    assert_eq!(
        attempt_sources
            .iter()
            .map(|(_, status, _)| *status)
            .collect::<Vec<_>>(),
        vec![
            sigil_kernel::PlanReviewAttemptStatus::Started,
            sigil_kernel::PlanReviewAttemptStatus::DraftReady,
        ]
    );
    assert_eq!(
        draft_index.expect("draft source") + 1,
        attempt_sources[1].0,
        "the draft plus DraftReady attempt are one unbroken control batch"
    );
    for (source_index, _, domain_event_id) in &attempt_sources {
        let paired = plan_changes
            .iter()
            .find(|entry| entry.domain_event_id == *domain_event_id)
            .expect("the ordinary Attempt source must have a same-bundle outbox DTO");
        let public_record = records
            .get(source_index + 1)
            .expect("outbox entry follows the source in the same writer bundle")
            .stored_event();
        assert_eq!(public_record.event_id.to_string(), paired.public_event_id);
        assert_eq!(
            public_record
                .causation_id
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some(domain_event_id.as_str())
        );
    }
    assert_eq!(
        recorder
            .0
            .iter()
            .filter(|event| matches!(event.event, PublicRunEventKind::PlanReviewChanged { .. }))
            .count(),
        2
    );
    Ok(())
}

#[tokio::test]
async fn explicit_plan_review_candidate_publication_failure_preserves_its_started_marker()
-> Result<()> {
    struct CandidatePublicationConflict<'a> {
        inner: PublicApplicationEventBridge<'a, RecordingApplicationRunEvents>,
        store: JsonlSessionStore,
        session_id: String,
        run_id: String,
        inserted: bool,
        reached_candidate: bool,
    }

    impl EventHandler for CandidatePublicationConflict<'_> {
        fn handle(&mut self, event: RunEvent) -> Result<()> {
            EventHandler::handle(&mut self.inner, event)
        }

        fn commit_controls(
            &mut self,
            session: &mut Session,
            controls: Vec<ControlEntry>,
        ) -> Result<Vec<sigil_kernel::StoredEvent>> {
            let candidate = controls
                .iter()
                .any(|control| matches!(control, ControlEntry::PlanReviewCandidateRecordedV1(_)));
            if candidate {
                self.reached_candidate = true;
                let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
                    &JsonlSessionStore::read_event_records(self.store.path())?,
                )?;
                let sequence = projection
                    .events_in_order()
                    .into_iter()
                    .filter(|entry| entry.run_id == self.run_id)
                    .map(|entry| entry.sequence)
                    .max()
                    .unwrap_or(0)
                    .checked_add(1)
                    .expect("test public sequence must remain representable");
                let public_event_id = format!("plan-review-conflicting-public:{sequence}");
                let foreign = PublicRunEvent::new(
                    &self.session_id,
                    &self.run_id,
                    sequence,
                    PublicRunEventKind::Notice {
                        message: "competing public sequence before candidate publication"
                            .to_owned(),
                    },
                );
                sigil_kernel::PublicEventOutboxRecorder::new(self.store.clone()).append_outbox(
                    &sigil_kernel::PublicEventOutboxEntryV1 {
                        schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
                        public_event_id: public_event_id.clone(),
                        domain_event_id: public_event_id,
                        run_id: self.run_id.clone(),
                        sequence,
                        payload_digest: sigil_kernel::stable_event_hash(serde_json::to_vec(
                            &foreign,
                        )?),
                        event: foreign,
                    },
                )?;
                self.inserted = true;
            }
            EventHandler::commit_controls(&mut self.inner, session, controls)
        }
    }

    impl ApplicationRunEventHandler for CandidatePublicationConflict<'_> {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.inner.handler.handle_public_event(event)
        }

        fn public_event_adapter_id(&self) -> &'static str {
            self.inner.handler.public_event_adapter_id()
        }
    }

    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let root_config = RootConfig::load(&config_path)?;
    let session_path = temp.path().join("plan-review-candidate.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session = Session::load_from_store("deepseek", "test-model", store.clone())?;
    let run_id = "run-explicit-plan-candidate-publication-failure";
    start_application_public_control_run(&session, run_id)?;
    let request = crate::PlanReviewCoordinator::prepare_explicit_plan_review(
        &mut session,
        "prove candidate publication does not manufacture a terminal",
        run_id,
        None,
        1,
    )?;
    let plan_review_id = request.plan_review_id.clone();
    let options = crate::build_run_options(
        &root_config,
        temp.path().to_path_buf(),
        InteractionMode::Headless,
        None,
    );
    let runtime = super::ApplicationPlanReviewRuntime {
        options,

        agent: Box::new(sigil_kernel::Agent::new(
            Box::new(PlanReviewCandidateProvider),
            ToolRegistry::new(),
        )),
        tool_registry: ToolRegistry::new(),
        workspace_snapshot_id: None,
        child_resource_provisioner: None,
    };
    let parent_output = AgentRunOutput {
        disposition: AgentRunDisposition::FinalAnswer,
        result: AgentRunResult {
            final_text: String::new(),
            tool_calls: 0,
            final_message_id: None,
        },
        outcome: AgentRunOutcome::default(),
    };
    let mut recorder = RecordingApplicationRunEvents::default();
    let bridge = PublicApplicationEventBridge::new(
        durable_application_event_sequence(session.session_scope_id(), run_id, &session_path)?,
        &mut recorder,
    )?;
    let mut handler = CandidatePublicationConflict {
        inner: bridge,
        store,
        session_id: session.session_scope_id().to_owned(),
        run_id: run_id.to_owned(),
        inserted: false,
        reached_candidate: false,
    };
    let cancellation_owner = RunCancellationOwner::new();
    let error = super::run_application_plan_review_request(
        &mut session,
        parent_output,
        runtime,
        request,
        &mut handler,
        &mut AutoApproveHandler,
        &cancellation_owner.handle(),
        run_id,
        &sigil_kernel::SecretRedactor::empty(),
    )
    .await
    .expect_err("a stale candidate outbox sequence must stop the original execution");
    assert!(
        handler.reached_candidate,
        "the real child produced a candidate"
    );
    assert!(
        handler.inserted,
        "inject only at the candidate control commit"
    );
    assert!(
        is_application_public_outbox_append_error(&error),
        "{error:#}"
    );
    drop(handler);
    assert!(
        recorder
            .0
            .iter()
            .all(|event| !matches!(event.event, PublicRunEventKind::RunFailed { .. }))
    );

    let records = JsonlSessionStore::read_event_records(&session_path)?;
    let entries = records
        .iter()
        .filter_map(|record| record.session_log_entry().transpose())
        .collect::<Result<Vec<_>>>()?;
    let attempts = entries
        .into_iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
                if attempt.plan_review_id == plan_review_id =>
            {
                Some(attempt.status)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        attempts,
        vec![sigil_kernel::PlanReviewAttemptStatus::Started],
        "the durable Started marker remains, but publication failure is not a domain Failed attempt"
    );
    assert!(records.iter().all(|record| {
        record.stored_event().event_kind() != Some(sigil_kernel::DurableEventType::RunFinalized)
    }));
    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    assert!(
        outbox
            .events_in_order()
            .iter()
            .all(|entry| { !matches!(entry.event.event, PublicRunEventKind::RunFailed { .. }) })
    );
    assert!(outbox.events_in_order().iter().any(|entry| {
        matches!(
            &entry.event.event,
            PublicRunEventKind::PlanReviewChanged { plan_review_id: observed, status, .. }
                if observed == plan_review_id.as_str()
                    && *status == sigil_kernel::PublicPlanReviewStatus::Started
        )
    }));
    Ok(())
}

#[tokio::test]
async fn run_pending_plan_route_drives_adoption_admission_and_terminal_synthesis() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_application_test_config(&config_path)?;
    let mut root_config = RootConfig::load(&config_path)?;
    root_config.task.enabled = true;
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let session_path = temp.path().join(".sigil/sessions/session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session =
        Session::load_from_store("application-task-test", "application-task-model", store)?;
    let base_snapshot = crate::plan_handoff_workspace_snapshot_id(&root_config, temp.path())?;
    let source_turn = sigil_kernel::ConversationTurnRef::new(
        session.session_scope_id(),
        "message-pending-plan-route",
        "run-pending-plan-route",
    )?;
    let mut source_message = ModelMessage::user("inspect the pending plan route");
    source_message.id = source_turn.message_id.clone();
    session.append_user_message(source_message)?;
    let request = crate::PlanReviewRunRequest {
        application_operation: None,
        plan_review_id: sigil_kernel::PlanReviewId::new("review-pending-plan-route")?,
        attempt_id: sigil_kernel::PlanReviewAttemptId::new("attempt-pending-plan-route")?,
        plan_id: sigil_kernel::PlanId::new("plan_pending_route")?,
        source: sigil_kernel::PlanReviewSource::AutomaticConversationRoute,
        source_turn: source_turn.clone(),
        route_decision_id: None,
        child_session_ref: SessionRef::new_relative("child.jsonl")?,
        revision_request_id: None,
        revision_generation: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        explicit_objective: None,
        objective: "inspect the pending plan route".to_owned(),
        workspace_snapshot_id: base_snapshot.clone(),
    };
    let mut plan_review_handler = NoopEventHandler;
    crate::PlanReviewCoordinator::ensure_attempt_started(
        &mut session,
        &request,
        &mut plan_review_handler,
        1,
    )?;
    let draft = sigil_kernel::plan_draft_created_entry_with_plan_id(
        request.plan_id.clone(),
        r#"```sigil-plan-v2
{"summary":"Inspect the pending plan route","steps":[{"step_id":"inspect","title":"Inspect","role":"executor","depends_on":[],"mode":"read","isolation":"shared_read_only","target_paths":["session.jsonl"],"deliverables":["Report the pending plan route inspection."]}]}
```"#,
        request.plan_source_ref(),
        2,
        base_snapshot,
    )?
    .expect("structured plan draft");
    crate::PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &mut plan_review_handler,
        2,
    )?;
    let plan_id = request.plan_id.clone();
    let plan_hash = draft.plan_hash.clone();
    let profile_registry =
        crate::AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &root_config,
            temp.path(),
            session.entries(),
        )?;
    let task_execution = super::ApplicationTaskExecutionRuntime {
        root_config: root_config.clone(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        options: crate::build_run_options(
            &root_config,
            temp.path().to_path_buf(),
            InteractionMode::Headless,
            None,
        ),
        base_registry: {
            let mut registry = ToolRegistry::new();
            registry.register(Arc::new(NamedTool("read_file")));
            registry
        },
        agent_supervisor: crate::AgentSupervisor::new(
            profile_registry,
            crate::AgentBudgetPolicy::from_root_config(&root_config),
            application_task_provider_capabilities(),
        ),
        role_provider_builder: Arc::new(ApplicationTaskRoleProviderBuilder),
        verification_execution_port: Some(Arc::new(LocalExecutionBackend)),
    };
    let cancellation_owner = RunCancellationOwner::new();
    let cancellation_handle = cancellation_owner.handle();
    let action = sigil_kernel::RunPendingPlanAction {
        plan_id: plan_id.clone(),
        plan_hash: plan_hash.clone(),
        source_turn,
    };
    let root_output = AgentRunOutput {
        disposition: AgentRunDisposition::RunPendingPlan(action),
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
    let output = super::continue_application_task_handoff(
        &mut session,
        root_output,
        Some(task_execution),
        &mut handler,
        &mut approval_handler,
        &cancellation_handle,
    )
    .await?;
    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    // Approval atomically created one stable Task plus first-class direct execution authority.
    let artifacts = session.plan_artifact_projection();
    let task_link = artifacts
        .tasks_created
        .get(&plan_id)
        .and_then(|entries| entries.first())
        .expect("model route must create the approved Task");
    assert_eq!(task_link.plan_hash, plan_hash);
    let tasks = session.task_state_projection();
    assert_eq!(
        tasks.tasks.get(&task_link.task_id).map(|task| task.status),
        Some(TaskRunStatus::Completed)
    );
    let task = tasks.tasks.get(&task_link.task_id).expect("direct Task");
    assert!(task.plans.is_empty());
    assert!(task.direct_execution_admission.is_some());
    assert_eq!(task.direct_execution_attempts.len(), 1);
    assert!(output.result.final_message_id.is_some());
    Ok(())
}

#[tokio::test]
async fn run_pending_plan_route_does_not_require_materialization_admission() -> Result<()> {
    let temp = tempfile::tempdir()?;
    // A missing planner connection must not block a Plan that can execute through its already
    // selected role provider. The old materialization admission incorrectly stopped here.
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "missing-connection"
model = "missing-model"

[task]
enabled = true
"#,
    )?;
    let root_config = RootConfig::load(&config_path)?;
    let session_path = temp.path().join(".sigil/sessions/session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session =
        Session::load_from_store("application-task-test", "application-task-model", store)?;
    let base_snapshot = crate::plan_handoff_workspace_snapshot_id(&root_config, temp.path())?;
    let source_turn = sigil_kernel::ConversationTurnRef::new(
        session.session_scope_id(),
        "message-pending-plan-blocked",
        "run-pending-plan-blocked",
    )?;
    let mut source_message = ModelMessage::user("inspect the blocked pending plan route");
    source_message.id = source_turn.message_id.clone();
    session.append_user_message(source_message)?;
    let request = crate::PlanReviewRunRequest {
        application_operation: None,
        plan_review_id: sigil_kernel::PlanReviewId::new("review-pending-plan-blocked")?,
        attempt_id: sigil_kernel::PlanReviewAttemptId::new("attempt-pending-plan-blocked")?,
        plan_id: sigil_kernel::PlanId::new("plan_pending_blocked")?,
        source: sigil_kernel::PlanReviewSource::AutomaticConversationRoute,
        source_turn: source_turn.clone(),
        route_decision_id: None,
        child_session_ref: SessionRef::new_relative("child.jsonl")?,
        revision_request_id: None,
        revision_generation: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        explicit_objective: None,
        objective: "inspect the blocked pending plan route".to_owned(),
        workspace_snapshot_id: base_snapshot.clone(),
    };
    let mut plan_review_handler = NoopEventHandler;
    crate::PlanReviewCoordinator::ensure_attempt_started(
        &mut session,
        &request,
        &mut plan_review_handler,
        1,
    )?;
    let draft = sigil_kernel::plan_draft_created_entry_with_plan_id(
        request.plan_id.clone(),
        r#"```sigil-plan-v2
{"summary":"Inspect the blocked route","steps":[{"step_id":"inspect","title":"Inspect","role":"executor","depends_on":[],"mode":"read","isolation":"shared_read_only","target_paths":["session.jsonl"],"deliverables":["Report the blocked pending plan route inspection."]}]}
```"#,
        request.plan_source_ref(),
        2,
        base_snapshot,
    )?
    .expect("structured plan draft");
    crate::PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &mut plan_review_handler,
        2,
    )?;
    let plan_id = request.plan_id.clone();
    let plan_hash = draft.plan_hash.clone();
    let profile_registry =
        crate::AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &root_config,
            temp.path(),
            session.entries(),
        )?;
    let task_execution = super::ApplicationTaskExecutionRuntime {
        root_config: root_config.clone(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        options: crate::build_run_options(
            &root_config,
            temp.path().to_path_buf(),
            InteractionMode::Headless,
            None,
        ),
        base_registry: {
            let mut registry = ToolRegistry::new();
            registry.register(Arc::new(NamedTool("read_file")));
            registry
        },
        agent_supervisor: crate::AgentSupervisor::new(
            profile_registry,
            crate::AgentBudgetPolicy::from_root_config(&root_config),
            application_task_provider_capabilities(),
        ),
        role_provider_builder: Arc::new(ApplicationTaskRoleProviderBuilder),
        verification_execution_port: Some(Arc::new(LocalExecutionBackend)),
    };
    let cancellation_owner = RunCancellationOwner::new();
    let cancellation_handle = cancellation_owner.handle();
    let action = sigil_kernel::RunPendingPlanAction {
        plan_id: plan_id.clone(),
        plan_hash: plan_hash.clone(),
        source_turn,
    };
    let root_output = AgentRunOutput {
        disposition: AgentRunDisposition::RunPendingPlan(action),
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
    let output = super::continue_application_task_handoff(
        &mut session,
        root_output,
        Some(task_execution),
        &mut handler,
        &mut approval_handler,
        &cancellation_handle,
    )
    .await?;
    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert_eq!(output.result.final_text, "application task step completed");
    let artifacts = session.plan_artifact_projection();
    let task_link = artifacts
        .tasks_created
        .get(&plan_id)
        .and_then(|entries| entries.first())
        .expect("model route must create the approved Task");
    let tasks = session.task_state_projection();
    assert_eq!(
        tasks.execution_phase(&task_link.task_id),
        Some(sigil_kernel::TaskExecutionPhaseV1::Completed)
    );
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Serializes CA environment reads during provider preparation.
async fn r71_application_prepare_injects_composed_tool_authority() -> Result<()> {
    let _environment_guard = crate::test_env::lock();
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let services = with_application_test_managed_authority(
        temp.path(),
        ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter)),
    )?;
    let managed_path = services
        .authority_composition()
        .expect("real boot composition")
        .storage_writer
        .session_log_path_for_key("composed-authority")?;
    let mut request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "inspect the workspace",
        "run-composed-authority",
    );
    request.session_path = Some(temp.path().join("composed-authority.jsonl"));
    let prepared = prepare_application_run(request, &services).await?;
    assert!(
        prepared.run_options().tool_authority.is_some(),
        "a new-epoch binary must hand the composed tool authority to the agent run"
    );
    assert_eq!(
        prepared.session_log_path(),
        managed_path.join("records.jsonl"),
        "current-schema runs must use the composed managed SessionLog source"
    );
    Ok(())
}

#[tokio::test]
async fn r71_application_prepare_rejects_legacy_composition() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let state = temp.path().join("state");
    let exec = temp.path().join("exec");
    std::fs::create_dir_all(&state)?;
    std::fs::create_dir_all(state.join("cache"))?;
    std::fs::create_dir_all(&exec)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
        std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o700))?;
    }
    let planner: std::sync::Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
        std::sync::Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        ));
    let composition = crate::r71_authority_composition::compose_runtime_authority(
        &state,
        &exec,
        sigil_kernel::resource::CanonicalHash::from_bytes([0x5b; 32]),
        sigil_kernel::resource::AuthorityGeneration {
            epoch: 1,
            instance_hash: sigil_kernel::resource::CanonicalHash::from_bytes([0x75; 32]),
        },
        planner,
        &[crate::managed_storage_writer::StorageWriterChannelV1::SessionLog],
    )?;
    let cutover = crate::r71_global_cutover::RuntimeGlobalCutoverV1::legacy_decision(
        "inst-apprun-legacy",
        1,
        sigil_kernel::resource::AuthorityGeneration {
            epoch: 0,
            instance_hash: sigil_kernel::resource::CanonicalHash::from_bytes([0x72; 32]),
        },
    );
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter))
        .with_global_cutover(cutover)
        .with_authority_composition(composition);
    let requested_path = temp.path().join("legacy-session.jsonl");
    let managed_path = services
        .authority_composition()
        .expect("the rejected fixture still has a physical authority")
        .storage_writer
        .session_log_path_for_key("legacy-session")?;
    assert!(!requested_path.exists());
    assert!(!managed_path.exists());
    let mut request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "inspect the workspace",
        "run-legacy-authority",
    );
    request.session_path = Some(requested_path.clone());
    let error = match prepare_application_run(request, &services).await {
        Ok(_) => anyhow::bail!("a legacy composition cannot prepare an application run"),
        Err(error) => error,
    };
    let ApplicationRunPrepareError::Configuration { source } = error else {
        anyhow::bail!("legacy epoch must produce its typed configuration refusal: {error:?}");
    };
    assert!(matches!(
        source.downcast_ref::<sigil_kernel::cutover_manifest::CutoverErrorV1>(),
        Some(sigil_kernel::cutover_manifest::CutoverErrorV1::LegacySessionUnavailable)
    ));
    assert!(!requested_path.exists());
    assert!(!managed_path.exists());
    Ok(())
}

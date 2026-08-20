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
    AgentRunResult, AgentRunTerminalReason, ApprovalHandler, AssistantMessageKind,
    AutoApproveHandler, CompletionRequest, ContextBodyRef, ContextInclusionReason, ContextItem,
    ContextSensitivity, ContextSource, ContextTrustLevel, ControlEntry,
    ConversationRunLifecycleRecordV1, ConversationRunStartedEntryV1,
    ConversationRunTerminalStatusV1, DisclosurePresentationError, DisclosurePresentationReceipt,
    EgressDisclosurePresenter, EventHandler, IntegrationPlanId, InteractionMode, JsonlSessionStore,
    MemoryConfig, ModelMessage, NoopEventHandler, PreEgressDisclosure, Provider,
    ProviderCapabilities, ProviderChunk, PublicRunEvent, PublicRunEventKind, ReasoningEffort,
    ReasoningStreamSupport, RootConfig, RunCancellationOwner, RunCancellationRequestedEntry,
    RunCancellationTarget, RunCancellationTerminalOutcome, RunEvent, RuntimeContextCandidates,
    Session, SessionLogEntry, SessionRef, StartDurableTaskAction, StartPlanReviewAction,
    TASK_GUIDANCE_APPLY_TOOL_NAME, TASK_PLAN_UPDATE_TOOL_NAME, TaskHandoffId, TaskId,
    TaskIntegrationReviewRequest, TaskPauseRequest, TaskPlanEntry, TaskPlanStatus,
    TaskRoutingPolicy, TaskRunCancellationScopeBoundEntry, TaskRunEntry, TaskRunStatus,
    TaskStepEntry, TaskStepId, TaskStepStatus, TaskVerificationRerunRequest, Tool, ToolAccess,
    ToolApproval, ToolArtifactSensitivity, ToolArtifactStore, ToolCall, ToolCategory, ToolContext,
    ToolExecutionEntry, ToolExecutionStatus, ToolPreviewCapability, ToolRegistry,
    ToolRegistryScope, ToolResult, ToolResultMeta, ToolResultRecordedV3, ToolSpec, UsageStats,
    UserInputActionV1, UserInputAnswerV1, UserInputAnswerValueV1, UserInputCommandId,
    UserInputContinuationBindingV1, UserInputDecisionV1, UserInputFieldKindV1, UserInputIdentityV1,
    UserInputLifecycleEntryV1, UserInputPurposeV1, UserInputQuestionV1, UserInputRequestId,
    UserInputRequestV1, UserInputRequestedV1, UserInputResolutionV1, UserInputSourceV1,
    UserInputStatusV1, conversation_run_lifecycle_record_from_stream,
};

use crate::agent_supervisor::task_role_runtime::TaskRoleProviderBuilder;

use super::{
    ApplicationCancellationTicket, ApplicationRunControl, ApplicationRunEventHandler,
    ApplicationRunEventSequence, ApplicationRunExecutionKind, ApplicationRunInteraction,
    ApplicationRunPrepareError, ApplicationRunPrepareErrorClass, ApplicationRunRequest,
    ApplicationRunServices, ApplicationRunTerminalStatus, ApplicationSessionLeaseManager,
    ApplicationTaskContinuationRequest, ApplicationTaskExecutionRuntime,
    ApplicationTaskPauseTicket, ApplicationTranscriptRole, ApplicationUserInputDecisionRequest,
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
    prepare_application_user_input_decision, record_application_preparation_cancellation,
    rerun_application_verification, validate_execution_contract,
};

fn application_conversation_lifecycle(
    path: &Path,
) -> Result<Vec<ConversationRunLifecycleRecordV1>> {
    JsonlSessionStore::read_event_records(path)?
        .iter()
        .filter_map(|record| conversation_run_lifecycle_record_from_stream(record).transpose())
        .collect()
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
    plan_version: u32,
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
        args_json: r#"{"prompt":"Choose a runtime mode","questions":[{"id":"mode","header":"Mode","question":"Which mode should continue?","required":true,"field":{"kind":"text","multiline":false,"max_chars":32}}]}"#.to_owned(),
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
            header: "Mode".to_owned(),
            question: "Which mode should continue?".to_owned(),
            description: None,
            required: true,
            field: UserInputFieldKindV1::Text {
                multiline: false,
                max_chars: 32,
            },
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
    let session_path = temp.path().join("state/sessions/user-input.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    let requested = seed_application_user_input_request(&config_path, temp.path(), &binding)?;
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter));

    let prepared = prepare_application_user_input_decision(
        ApplicationUserInputDecisionRequest {
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
    let error = execution
        .execute(&mut events, &mut approvals)
        .await
        .expect_err("closed local endpoint should fail after continuation dispatch");
    assert!(!error.to_string().is_empty());
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
    let session_path = temp
        .path()
        .join("state/sessions/user-input-provider-failure.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    let requested = seed_application_user_input_request(&config_path, temp.path(), &binding)?;
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter));
    let command_id = UserInputCommandId::new("user-input-provider-failure-command")?;

    let error = match prepare_application_user_input_decision(
        ApplicationUserInputDecisionRequest {
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

struct QuestioningApplicationTaskRoleProviderBuilder {
    planner_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl TaskRoleProviderBuilder for ApplicationTaskRoleProviderBuilder {
    async fn build(&self, _root_config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
        Ok(Box::new(ApplicationTaskRoleProvider { role }))
    }
}

#[async_trait]
impl TaskRoleProviderBuilder for QuestioningApplicationTaskRoleProviderBuilder {
    async fn build(&self, _root_config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
        Ok(Box::new(QuestioningApplicationTaskRoleProvider {
            role,
            planner_calls: Arc::clone(&self.planner_calls),
        }))
    }
}

struct ApplicationTaskRoleProvider {
    role: AgentRole,
}

struct QuestioningApplicationTaskRoleProvider {
    role: AgentRole,
    planner_calls: Arc<AtomicUsize>,
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
                .any(|tool| tool.name == TASK_GUIDANCE_APPLY_TOOL_NAME)
        {
            let args = r#"{
                "reason": "prioritizes_pending_step",
                "target_step_ids": ["inspect_application"]
            }"#;
            vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "application-task-guidance".to_owned(),
                    name: TASK_GUIDANCE_APPLY_TOOL_NAME.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "application-task-guidance".to_owned(),
                    delta: args.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "application-task-guidance".to_owned(),
                    name: TASK_GUIDANCE_APPLY_TOOL_NAME.to_owned(),
                    args_json: args.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ]
        } else if self.role == AgentRole::Planner
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

#[async_trait]
impl Provider for QuestioningApplicationTaskRoleProvider {
    fn name(&self) -> &str {
        "questioning-application-task-test"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        application_task_provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let is_planning = self.role == AgentRole::Planner
            && request
                .tools
                .iter()
                .any(|tool| tool.name == TASK_PLAN_UPDATE_TOOL_NAME);
        let chunks = if is_planning && self.planner_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let args = r#"{
                "prompt": "Choose the application subsystem",
                "questions": [{
                    "id": "scope",
                    "header": "Scope",
                    "question": "Which subsystem should the task inspect?",
                    "required": true,
                    "field": {
                        "kind": "text",
                        "multiline": false,
                        "max_chars": 128
                    }
                }]
            }"#;
            vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "application-task-question".to_owned(),
                    name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "application-task-question".to_owned(),
                    delta: args.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "application-task-question".to_owned(),
                    name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
                    args_json: args.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ]
        } else if is_planning {
            let args = r#"{
                "plan_version": 1,
                "status": "accepted",
                "steps": [{
                    "step_id": "inspect_application",
                    "title": "Inspect the selected application subsystem",
                    "role": "executor",
                    "mode": "read",
                    "isolation": "shared_read_only"
                }]
            }"#;
            vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "application-task-plan-after-answer".to_owned(),
                    name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "application-task-plan-after-answer".to_owned(),
                    delta: args.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "application-task-plan-after-answer".to_owned(),
                    name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
                    args_json: args.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ]
        } else if self.role == AgentRole::Planner {
            vec![
                Ok(ProviderChunk::TextDelta(
                    "application task completed after clarification".to_owned(),
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

struct CapturingApplicationTaskRoleProviderBuilder {
    executor_requests: Arc<Mutex<Vec<CompletionRequest>>>,
    guidance_review_requests: Arc<AtomicUsize>,
}

#[async_trait]
impl TaskRoleProviderBuilder for CapturingApplicationTaskRoleProviderBuilder {
    async fn build(&self, _root_config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
        Ok(Box::new(CapturingApplicationTaskRoleProvider {
            role,
            executor_requests: Arc::clone(&self.executor_requests),
            guidance_review_requests: Arc::clone(&self.guidance_review_requests),
        }))
    }
}

struct CapturingApplicationTaskRoleProvider {
    role: AgentRole,
    executor_requests: Arc<Mutex<Vec<CompletionRequest>>>,
    guidance_review_requests: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for CapturingApplicationTaskRoleProvider {
    fn name(&self) -> &str {
        "capturing-application-task-test"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        application_task_provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        if self.role == AgentRole::Planner
            && request
                .tools
                .iter()
                .any(|tool| tool.name == TASK_GUIDANCE_APPLY_TOOL_NAME)
        {
            self.guidance_review_requests.fetch_add(1, Ordering::SeqCst);
            let args = r#"{
                "reason": "prioritizes_pending_step",
                "target_step_ids": ["step_2"]
            }"#;
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "application-guidance-recovery".to_owned(),
                    name: TASK_GUIDANCE_APPLY_TOOL_NAME.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "application-guidance-recovery".to_owned(),
                    delta: args.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "application-guidance-recovery".to_owned(),
                    name: TASK_GUIDANCE_APPLY_TOOL_NAME.to_owned(),
                    args_json: args.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }
        let text = if self.role == AgentRole::Executor {
            self.executor_requests
                .lock()
                .expect("executor request lock should not be poisoned")
                .push(request);
            "recovered application task step completed"
        } else {
            "recovered application task synthesis completed"
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(text.to_owned())),
            Ok(ProviderChunk::Done),
        ])))
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

#[derive(Default)]
struct RecordingRunEvents(Vec<RunEvent>);

impl EventHandler for RecordingRunEvents {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
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

#[tokio::test]
async fn builtin_plan_agent_binding_prepares_a_durable_explicit_review() -> Result<()> {
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

    let prepared = prepare_application_run(request, &services).await?;

    assert!(matches!(
        prepared.execution.kind,
        ApplicationRunExecutionKind::ExplicitPlanReview { .. }
    ));
    let projection =
        sigil_kernel::PlanReviewProjection::from_entries(prepared.execution.session.entries());
    let attempt = projection
        .reviews()
        .next()
        .and_then(sigil_kernel::PlanReviewProjectionEntry::latest_attempt)
        .expect("explicit plan preparation should append one attempt");
    assert_eq!(
        attempt.source,
        sigil_kernel::PlanReviewSource::ExplicitPlanCommand
    );
    assert_eq!(
        attempt.status,
        sigil_kernel::PlanReviewAttemptStatus::Started
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
async fn application_preparation_enables_model_owned_auto_handoff_without_host_classification()
-> Result<()> {
    // Attaching the task-role provider constructs the configured DeepSeek route during
    // preparation. Keep that credential lookup hermetic instead of inheriting a developer key or
    // racing another test's process-global environment mutation.
    let _environment_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-api-key");
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

    let root_config: RootConfig = toml::from_str(&std::fs::read_to_string(&config_path)?)?;
    let _rollout_guard =
        crate::tests::rollout_manifest_test_support::qualified_rollout_manifest_guard(&root_config);
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
        title: None,

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
        workspace_root: temp.path().to_path_buf(),
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

#[test]
fn application_task_planner_question_resumes_through_the_public_decision_path() -> Result<()> {
    std::thread::Builder::new()
        .name("application-planner-input-recovery".to_owned())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| -> Result<()> {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(application_task_planner_question_resumes_through_the_public_decision_path_inner())
        })?
        .join()
        .map_err(|_| anyhow::anyhow!("application planner input recovery test panicked"))?
}

async fn application_task_planner_question_resumes_through_the_public_decision_path_inner()
-> Result<()> {
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
    let requested_session_path = temp.path().join("session.jsonl");
    let binding =
        bind_application_session(&config_path, temp.path(), Some(&requested_session_path))?;
    let session_path = binding.session_log_path.clone();
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session =
        Session::load_from_store("application-task-test", "application-task-model", store)?;
    let session_scope_id = binding.session_scope_id;
    let task_id = TaskId::new("task-application-planner-question")?;
    let parent_session_ref = SessionRef::new_relative("session.jsonl")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref,
        objective: "inspect an application subsystem after clarification".to_owned(),
        title: None,
        status: TaskRunStatus::Started,
        reason: Some("accepted by the application conversation coordinator".to_owned()),
    }))?;
    let profile_registry =
        crate::AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &root_config,
            temp.path(),
            session.entries(),
        )?;
    let planner_calls = Arc::new(AtomicUsize::new(0));
    let provider_builder: Arc<dyn TaskRoleProviderBuilder> =
        Arc::new(QuestioningApplicationTaskRoleProviderBuilder {
            planner_calls: Arc::clone(&planner_calls),
        });
    let task_execution = ApplicationTaskExecutionRuntime {
        root_config: root_config.clone(),
        workspace_root: temp.path().to_path_buf(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        options: crate::build_run_options(
            &root_config,
            temp.path().to_path_buf(),
            InteractionMode::Headless,
            None,
        ),
        base_registry: ToolRegistry::new(),
        agent_supervisor: crate::AgentSupervisor::new(
            profile_registry,
            crate::AgentBudgetPolicy::from_root_config(&root_config),
            application_task_provider_capabilities(),
        ),
        role_provider_builder: Arc::clone(&provider_builder),
    };
    let cancellation_owner = RunCancellationOwner::new();
    let cancellation_handle = cancellation_owner.handle();
    let action = StartDurableTaskAction {
        handoff_id: TaskHandoffId::new("handoff-application-planner-question")?,
        task_id: task_id.clone(),
        source_turn: sigil_kernel::ConversationTurnRef::new(
            &session_scope_id,
            "message-application-planner-question",
            "run-application-planner-question",
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

    let suspended = Box::pin(continue_application_task_handoff(
        &mut session,
        root_output,
        Some(task_execution),
        &mut handler,
        &mut approval_handler,
        &cancellation_handle,
    ))
    .await?;
    let AgentRunDisposition::AwaitingUserInput(request_ref) = suspended.disposition else {
        panic!("application task planner must surface its question to the root run");
    };
    let route =
        sigil_kernel::AgentUserInputRouteProjectionV1::from_session_entries(session.entries())?
            .pending()
            .next()
            .cloned()
            .expect("planner question must have a root attention route");
    assert_eq!(route.request.identity, request_ref.identity);
    assert_eq!(route.request.request_hash, request_ref.request_hash);
    assert_eq!(
        session
            .task_state_projection()
            .tasks
            .get(&task_id)
            .map(|task| task.status),
        Some(TaskRunStatus::Paused)
    );
    drop(session);

    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter))
        .with_task_role_provider_builder(provider_builder);
    let command = sigil_kernel::UserInputDecisionCommandV1 {
        identity: route.request.identity.clone(),
        request_hash: route.request.request_hash.clone(),
        command_id: UserInputCommandId::new("application-planner-answer-command")?,
        decision: UserInputDecisionV1::Submitted {
            answers: vec![UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: UserInputAnswerValueV1::Text {
                    value: "runtime".to_owned(),
                },
            }],
        },
    };
    let stranded = Box::pin(prepare_application_user_input_decision(
        ApplicationUserInputDecisionRequest {
            config_path: config_path.clone(),
            launch_cwd: temp.path().to_path_buf(),
            session_path: session_path.clone(),
            session_attachment: None,
            expected_session_scope_id: session_scope_id.clone(),
            run_id: "application-planner-answer-run".to_owned(),
            identity: command.identity.clone(),
            request_hash: command.request_hash.clone(),
            command_id: command.command_id.clone(),
            decision: command.decision.clone(),
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    ))
    .await?;
    assert!(stranded.has_continuation());
    drop(stranded);

    let recovered_command = crate::application_run::application_recoverable_user_input_decision(
        &session_path,
        &session_scope_id,
    )?
    .expect("accepted planner answer must survive a controller crash before registration");
    assert_eq!(recovered_command, command);
    let recovered_request = crate::application_run::application_user_input_request_view(
        &session_path,
        &session_scope_id,
        &command.identity,
        &command.request_hash,
    )?;
    assert_eq!(
        recovered_request.status,
        sigil_kernel::UserInputStatusV1::DecisionAccepted
    );

    let prepared = Box::pin(prepare_application_user_input_decision(
        ApplicationUserInputDecisionRequest {
            config_path,
            launch_cwd: temp.path().to_path_buf(),
            session_path: session_path.clone(),
            session_attachment: None,
            expected_session_scope_id: session_scope_id.clone(),
            run_id: "application-planner-answer-recovery-run".to_owned(),
            identity: recovered_command.identity,
            request_hash: recovered_command.request_hash,
            command_id: recovered_command.command_id,
            decision: recovered_command.decision,
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    ))
    .await?;
    assert!(prepared.has_continuation());
    assert!(prepared.receipt().continuation_required);
    let (_, continuation, revision) = prepared.into_parts();
    assert!(revision.is_none());
    let (execution, control) = continuation
        .expect("submitted planner answer must prepare a supervised continuation")
        .into_parts();
    let mut events = RecordingApplicationRunEvents::default();
    let completed = Box::pin(execution.execute(&mut events, &mut approval_handler)).await?;
    drop(control);

    assert_eq!(planner_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        completed.agent_output.disposition,
        AgentRunDisposition::FinalAnswer
    );
    assert_eq!(
        completed.agent_output.result.final_text,
        "application task completed after clarification"
    );
    let recovered = Session::load_from_store(
        "application-task-test",
        "application-task-model",
        JsonlSessionStore::new(&session_path)?,
    )?;
    assert_eq!(
        recovered
            .task_state_projection()
            .tasks
            .get(&task_id)
            .map(|task| task.status),
        Some(TaskRunStatus::Completed)
    );
    assert_eq!(
        sigil_kernel::AgentUserInputRouteProjectionV1::from_session_entries(recovered.entries(),)?
            .route(&route.route_id)
            .map(|route| route.status),
        Some(sigil_kernel::AgentRouteStatus::Resolved)
    );
    assert!(
        events
            .0
            .iter()
            .any(|event| matches!(event.event, PublicRunEventKind::RunFinished { .. }))
    );
    Ok(())
}

#[tokio::test]
async fn application_typed_task_continuation_executes_exact_selected_task() -> Result<()> {
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
    let task_id = TaskId::new("task-application-selected-continuation")?;
    let decoy_task_id = TaskId::new("task-application-newer-decoy")?;
    let parent_session_ref = SessionRef::new_relative("session.jsonl")?;
    for (id, objective) in [
        (task_id.clone(), "finish the selected application task"),
        (decoy_task_id.clone(), "leave this newer task paused"),
    ] {
        session.append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: id.clone(),
            parent_session_ref: parent_session_ref.clone(),
            objective: objective.to_owned(),
            title: None,
            status: TaskRunStatus::Paused,
            reason: Some("waiting for a follow-up".to_owned()),
        }))?;
        session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: id,
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: TaskStepId::new("inspect_application")?,
                title: "Inspect application runtime".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: Some(sigil_kernel::TaskStepMode::Read),
                isolation: Some(sigil_kernel::TaskIsolationMode::SharedReadOnly),
            }],
            reason: None,
        }))?;
    }
    let source_turn = sigil_kernel::ConversationTurnRef::new(
        session.session_scope_id(),
        "message-application-selected-continuation",
        "run-application-selected-continuation",
    )?;
    let exact_guidance = "finish the task we were already working on";
    let mut message = ModelMessage::user(exact_guidance);
    message.id = source_turn.message_id.clone();
    session.append_user_message(message)?;
    let route_contract_fingerprint = "route-selected-application-task".to_owned();
    session.append_control(ControlEntry::ConversationRouteDecisionRecorded(
        sigil_kernel::ConversationRouteDecisionRecordedEntry {
            decision_id: sigil_kernel::conversation_route_decision_id_for_source(&source_turn),
            source_turn: source_turn.clone(),
            route: sigil_kernel::ConversationRoute::Task,
            reason_codes: Vec::new(),
            configured_policy: TaskRoutingPolicy::Auto,
            effective_capability: sigil_kernel::AutomaticRouteCapability::DirectTask,
            policy_snapshot_hash: "task-routing-policy".to_owned(),
            route_contract_fingerprint: route_contract_fingerprint.clone(),
            decided_at_ms: 1,
        },
    ))?;
    let guidance_projection =
        sigil_kernel::project_conversation_prompt_for_persistence(exact_guidance);
    let guidance_receipt = sigil_kernel::TaskContinuationSelectedEntry {
        task_id: task_id.clone(),
        source_turn: source_turn.clone(),
        plan_version: Some(1),
        task_status: TaskRunStatus::Paused,
        plan_status: Some(TaskPlanStatus::Accepted),
        route_contract_fingerprint: route_contract_fingerprint.clone(),
        control: sigil_kernel::TaskContinuationControlKind::ApplyCurrentRequestAsGuidance,
        prompt_hash: guidance_projection.prompt_hash,
        exact_prompt_required: guidance_projection.exact_prompt_required,
        guidance: guidance_projection.safe_prompt,
        selected_at_ms: 1,
    };
    session.append_control(ControlEntry::TaskContinuationSelected(
        guidance_receipt.clone(),
    ))?;
    let profile_registry =
        crate::AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &root_config,
            temp.path(),
            session.entries(),
        )?;
    let task_execution = ApplicationTaskExecutionRuntime {
        root_config: root_config.clone(),
        workspace_root: temp.path().to_path_buf(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        options: crate::build_run_options(
            &root_config,
            temp.path().to_path_buf(),
            InteractionMode::Headless,
            None,
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
    let root_output = AgentRunOutput {
        disposition: AgentRunDisposition::ContinueDurableTask(Box::new(
            sigil_kernel::ContinueDurableTaskAction {
                task_id: task_id.clone(),
                source_turn,
                plan_version: Some(1),
                task_status: TaskRunStatus::Paused,
                plan_status: Some(TaskPlanStatus::Accepted),
                route_contract_fingerprint,
                control: sigil_kernel::TaskContinuationControl::ApplyTaskGuidance(
                    exact_guidance.to_owned(),
                ),
                guidance: sigil_kernel::SecretString::new(exact_guidance),
                guidance_receipt,
            },
        )),
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
    assert!(cancellation_handle.is_naturally_finalized());
    let projection = session.task_state_projection();
    assert_eq!(
        projection.tasks.get(&task_id).map(|task| task.status),
        Some(TaskRunStatus::Completed)
    );
    assert_eq!(
        projection.tasks.get(&decoy_task_id).map(|task| task.status),
        Some(TaskRunStatus::Paused),
        "typed continuation must never fall back to a newer resumable Task"
    );
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskGuidanceApplied(applied))
            if applied.task_id == task_id
                && applied.target_step_ids
                    == vec![TaskStepId::new("inspect_application").expect("valid step id")]
    )));
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(bound))
                    if bound.task_id == task_id
            ))
            .count(),
        1
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
        title: None,
        status: TaskRunStatus::Paused,
        reason: Some("application restart".to_owned()),
    }))?;
    let unrelated_user = ModelMessage::user("explain an unrelated module first");
    let unrelated_user_id = unrelated_user.id.clone();
    session.append_user_message(unrelated_user)?;
    assert!(
        session.task_state_projection().current_task().is_none(),
        "an ordinary User turn must clear the previous Task focus before explicit continuation"
    );
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
            .current_task()
            .map(|task| &task.task_id),
        Some(&task_id)
    );
    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskRunTargetSelected(selected))
                    if selected.task_id == task_id
            ))
            .count(),
        1
    );
    Ok(())
}

struct ApplicationGuidanceRecoveryFixture {
    config_path: std::path::PathBuf,
    session_path: std::path::PathBuf,
    session_scope_id: String,
    task_id: TaskId,
    exact_guidance: String,
    safe_guidance: String,
}

#[derive(Clone, Copy)]
enum ApplicationGuidanceRecoveryBoundary {
    Materialized,
    SelectionOnly,
    SelectionWithStartedPlanner,
}

fn application_guidance_recovery_fixture(
    root: &Path,
    exact_prompt_required: bool,
    boundary: ApplicationGuidanceRecoveryBoundary,
) -> Result<ApplicationGuidanceRecoveryFixture> {
    let config_path = root.join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let session_path = root.join("session.jsonl");
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
    let task_id = TaskId::new(match (exact_prompt_required, boundary) {
        (true, ApplicationGuidanceRecoveryBoundary::Materialized) => {
            "task-application-guidance-exact-recovery"
        }
        (false, ApplicationGuidanceRecoveryBoundary::Materialized) => {
            "task-application-guidance-safe-recovery"
        }
        (true, ApplicationGuidanceRecoveryBoundary::SelectionOnly) => {
            "task-application-guidance-exact-selection-recovery"
        }
        (false, ApplicationGuidanceRecoveryBoundary::SelectionOnly) => {
            "task-application-guidance-safe-selection-recovery"
        }
        (true, ApplicationGuidanceRecoveryBoundary::SelectionWithStartedPlanner) => {
            "task-application-guidance-exact-started-recovery"
        }
        (false, ApplicationGuidanceRecoveryBoundary::SelectionWithStartedPlanner) => {
            "task-application-guidance-safe-started-recovery"
        }
    })?;
    let initial_task_status = if matches!(
        boundary,
        ApplicationGuidanceRecoveryBoundary::SelectionWithStartedPlanner
    ) {
        TaskRunStatus::Started
    } else {
        TaskRunStatus::Paused
    };
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        objective: "recover materialized application guidance".to_owned(),
        title: None,
        status: initial_task_status,
        reason: Some("crashed after guidance review".to_owned()),
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![
            sigil_kernel::TaskStepSpec {
                step_id: TaskStepId::new("step_1")?,
                title: "Inspect the baseline".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: Some(sigil_kernel::TaskStepMode::Read),
                isolation: Some(sigil_kernel::TaskIsolationMode::SharedReadOnly),
            },
            sigil_kernel::TaskStepSpec {
                step_id: TaskStepId::new("step_2")?,
                title: "Inspect the exact recovery target".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: vec![TaskStepId::new("step_1")?],
                intent_refs: Vec::new(),
                mode: Some(sigil_kernel::TaskStepMode::Read),
                isolation: Some(sigil_kernel::TaskIsolationMode::SharedReadOnly),
            },
        ],
        reason: None,
    }))?;
    session.append_user_message(ModelMessage::user("explain an unrelated module first"))?;

    let exact_guidance = if exact_prompt_required {
        "inspect step 2 with authorization=super-secret-value"
    } else {
        "prioritize the compatibility check in step 2"
    }
    .to_owned();
    let projected = sigil_kernel::project_conversation_prompt_for_persistence(&exact_guidance);
    assert_eq!(projected.exact_prompt_required, exact_prompt_required);
    match boundary {
        ApplicationGuidanceRecoveryBoundary::Materialized => {
            let applied = sigil_kernel::TaskGuidanceAppliedEntry {
                queue_id: sigil_kernel::ConversationInputQueueId::new(if exact_prompt_required {
                    "queue-application-guidance-exact-recovery"
                } else {
                    "queue-application-guidance-safe-recovery"
                })?,
                task_id: task_id.clone(),
                plan_version: 1,
                dispatch_run_id: if exact_prompt_required {
                    "dispatch-application-guidance-exact-recovery"
                } else {
                    "dispatch-application-guidance-safe-recovery"
                }
                .to_owned(),
                reason: sigil_kernel::TaskGuidanceApplyReason::PrioritizesPendingStep,
                target_step_ids: vec![TaskStepId::new("step_2")?],
            };
            let materialized = sigil_kernel::TaskGuidanceMaterializedEntry::new(
                &applied,
                projected.prompt_hash.clone(),
                projected.exact_prompt_required,
                projected.safe_prompt.clone(),
            )?;
            session.append_controls(vec![
                ControlEntry::TaskGuidanceApplied(applied),
                ControlEntry::TaskGuidanceMaterialized(materialized),
            ])?;
            assert!(session.task_state_projection().current_task().is_none());
        }
        ApplicationGuidanceRecoveryBoundary::SelectionOnly
        | ApplicationGuidanceRecoveryBoundary::SelectionWithStartedPlanner => {
            let source_turn = sigil_kernel::ConversationTurnRef::new(
                session.session_scope_id(),
                "message-application-guidance-selection-recovery",
                "run-application-guidance-selection-recovery",
            )?;
            let mut source_message = ModelMessage::user(projected.safe_prompt.clone());
            source_message.id = source_turn.message_id.clone();
            session.append_user_message(source_message)?;
            session.append_control(ControlEntry::TaskContinuationSelected(
                sigil_kernel::TaskContinuationSelectedEntry {
                    task_id: task_id.clone(),
                    plan_version: Some(1),
                    task_status: initial_task_status,
                    plan_status: Some(TaskPlanStatus::Accepted),
                    source_turn,
                    route_contract_fingerprint: "sha256:application-guidance-selection-recovery"
                        .to_owned(),
                    control:
                        sigil_kernel::TaskContinuationControlKind::ApplyCurrentRequestAsGuidance,
                    prompt_hash: projected.prompt_hash.clone(),
                    exact_prompt_required: projected.exact_prompt_required,
                    guidance: projected.safe_prompt.clone(),
                    selected_at_ms: 1,
                },
            ))?;
            if matches!(
                boundary,
                ApplicationGuidanceRecoveryBoundary::SelectionWithStartedPlanner
            ) {
                let attempt_id = sigil_kernel::task_participant_attempt_id(
                    &task_id,
                    sigil_kernel::TaskParticipantPurpose::Planner,
                    None,
                    None,
                    1,
                )?;
                session.append_control(ControlEntry::TaskParticipantAttempt(
                    sigil_kernel::TaskParticipantAttemptEntry {
                        child_session_ref: sigil_kernel::task_participant_session_ref(
                            &task_id,
                            &attempt_id,
                        )?,
                        attempt_id,
                        task_id: task_id.clone(),
                        purpose: sigil_kernel::TaskParticipantPurpose::Planner,
                        ordinal: 1,
                        plan_version: None,
                        step_id: None,
                        role: AgentRole::Planner,
                        status: sigil_kernel::TaskParticipantAttemptStatus::Started,
                        reason: None,
                    },
                ))?;
            }
            assert_eq!(
                session
                    .task_state_projection()
                    .current_task()
                    .map(|task| &task.task_id),
                Some(&task_id)
            );
        }
    }

    Ok(ApplicationGuidanceRecoveryFixture {
        config_path,
        session_path,
        session_scope_id: session.session_scope_id().to_owned(),
        task_id,
        exact_guidance,
        safe_guidance: projected.safe_prompt,
    })
}

#[tokio::test]
async fn application_continuation_recovers_safe_materialized_guidance_after_reload() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let fixture = application_guidance_recovery_fixture(
        temp.path(),
        false,
        ApplicationGuidanceRecoveryBoundary::Materialized,
    )?;
    let executor_requests = Arc::new(Mutex::new(Vec::new()));
    let guidance_review_requests = Arc::new(AtomicUsize::new(0));
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter))
        .with_task_role_provider_builder(Arc::new(CapturingApplicationTaskRoleProviderBuilder {
            executor_requests: Arc::clone(&executor_requests),
            guidance_review_requests: Arc::clone(&guidance_review_requests),
        }));
    let prepared = prepare_application_task_continuation(
        ApplicationTaskContinuationRequest {
            config_path: fixture.config_path,
            launch_cwd: temp.path().to_path_buf(),
            session_path: fixture.session_path,
            session_attachment: None,
            expected_session_scope_id: fixture.session_scope_id,
            run_id: "run-application-guidance-safe-recovery".to_owned(),
            task_id: fixture.task_id,
            guidance: None,
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    )
    .await?;
    let (execution, _control) = prepared.into_parts();
    let mut handler = RecordingApplicationRunEvents::default();
    let mut approval_handler = AutoApproveHandler;

    let output = execution
        .execute(&mut handler, &mut approval_handler)
        .await?;

    assert_eq!(output.task_status, TaskRunStatus::Completed);
    let prompts = executor_requests
        .lock()
        .expect("executor request lock should not be poisoned")
        .iter()
        .map(|request| {
            request
                .messages
                .iter()
                .filter_map(|message| message.content.as_deref())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect::<Vec<_>>();
    assert_eq!(prompts.len(), 2);
    let first = prompts
        .iter()
        .find(|prompt| prompt.contains("Step: step_1"))
        .expect("first executor step request");
    let second = prompts
        .iter()
        .find(|prompt| prompt.contains("Step: step_2"))
        .expect("second executor step request");
    assert!(!first.contains(&fixture.safe_guidance));
    assert!(second.contains(&fixture.safe_guidance));
    assert_eq!(guidance_review_requests.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn application_continuation_recovers_exact_required_materialized_guidance_after_reload()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let fixture = application_guidance_recovery_fixture(
        temp.path(),
        true,
        ApplicationGuidanceRecoveryBoundary::Materialized,
    )?;
    let session_path = fixture.session_path.clone();
    let exact_guidance = fixture.exact_guidance.clone();
    let executor_requests = Arc::new(Mutex::new(Vec::new()));
    let guidance_review_requests = Arc::new(AtomicUsize::new(0));
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter))
        .with_task_role_provider_builder(Arc::new(CapturingApplicationTaskRoleProviderBuilder {
            executor_requests: Arc::clone(&executor_requests),
            guidance_review_requests: Arc::clone(&guidance_review_requests),
        }));
    let prepared = prepare_application_task_continuation(
        ApplicationTaskContinuationRequest {
            config_path: fixture.config_path,
            launch_cwd: temp.path().to_path_buf(),
            session_path: fixture.session_path,
            session_attachment: None,
            expected_session_scope_id: fixture.session_scope_id,
            run_id: "run-application-guidance-exact-recovery".to_owned(),
            task_id: fixture.task_id,
            guidance: None,
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    )
    .await?;
    let (execution, _control) = prepared.into_parts();
    let mut handler = RecordingApplicationRunEvents::default();
    let mut approval_handler = AutoApproveHandler;

    let output = execution
        .execute(&mut handler, &mut approval_handler)
        .await?;
    assert_eq!(output.task_status, TaskRunStatus::Completed);
    let prompts = executor_requests
        .lock()
        .expect("executor request lock should not be poisoned")
        .iter()
        .map(|request| {
            request
                .messages
                .iter()
                .filter_map(|message| message.content.as_deref())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect::<Vec<_>>();
    assert_eq!(prompts.len(), 2);
    let first = prompts
        .iter()
        .find(|prompt| prompt.contains("Step: step_1"))
        .expect("first executor step request");
    let second = prompts
        .iter()
        .find(|prompt| prompt.contains("Step: step_2"))
        .expect("second executor step request");
    assert!(!first.contains(&fixture.safe_guidance));
    assert!(second.contains(&fixture.safe_guidance));
    assert!(!std::fs::read_to_string(session_path)?.contains(&exact_guidance));
    assert_eq!(guidance_review_requests.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn application_continuation_recovers_safe_selection_only_guidance_after_reload() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let fixture = application_guidance_recovery_fixture(
        temp.path(),
        false,
        ApplicationGuidanceRecoveryBoundary::SelectionOnly,
    )?;
    let session_path = fixture.session_path.clone();
    let task_id = fixture.task_id.clone();
    let executor_requests = Arc::new(Mutex::new(Vec::new()));
    let guidance_review_requests = Arc::new(AtomicUsize::new(0));
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter))
        .with_task_role_provider_builder(Arc::new(CapturingApplicationTaskRoleProviderBuilder {
            executor_requests: Arc::clone(&executor_requests),
            guidance_review_requests: Arc::clone(&guidance_review_requests),
        }));
    let prepared = prepare_application_task_continuation(
        ApplicationTaskContinuationRequest {
            config_path: fixture.config_path,
            launch_cwd: temp.path().to_path_buf(),
            session_path: fixture.session_path,
            session_attachment: None,
            expected_session_scope_id: fixture.session_scope_id,
            run_id: "run-application-guidance-safe-selection-recovery".to_owned(),
            task_id: fixture.task_id,
            guidance: None,
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    )
    .await?;
    let (execution, _control) = prepared.into_parts();
    let mut handler = RecordingApplicationRunEvents::default();
    let mut approval_handler = AutoApproveHandler;

    let output = execution
        .execute(&mut handler, &mut approval_handler)
        .await?;

    assert_eq!(output.task_status, TaskRunStatus::Completed);
    assert_eq!(guidance_review_requests.load(Ordering::SeqCst), 1);
    let prompts = executor_requests
        .lock()
        .expect("executor request lock should not be poisoned")
        .iter()
        .map(|request| {
            request
                .messages
                .iter()
                .filter_map(|message| message.content.as_deref())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect::<Vec<_>>();
    assert_eq!(prompts.len(), 2);
    let first = prompts
        .iter()
        .find(|prompt| prompt.contains("Step: step_1"))
        .expect("first executor step request");
    let second = prompts
        .iter()
        .find(|prompt| prompt.contains("Step: step_2"))
        .expect("second executor step request");
    assert!(!first.contains(&fixture.safe_guidance));
    assert!(second.contains(&fixture.safe_guidance));

    let reopened = Session::load_from_store(
        "deepseek",
        "deepseek-v4-flash",
        JsonlSessionStore::new(&session_path)?,
    )?;
    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskContinuationSelected(selected))
                    if selected.task_id == task_id
            ))
            .count(),
        1
    );
    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskGuidanceApplied(applied))
                    if applied.task_id == task_id
            ))
            .count(),
        1,
        "selection-only recovery must consume the existing authority exactly once"
    );
    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskGuidanceMaterialized(materialized))
                    if materialized.task_id == task_id
            ))
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn application_continuation_explicitly_retries_selection_owned_uncertain_planner_after_reload()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let fixture = application_guidance_recovery_fixture(
        temp.path(),
        false,
        ApplicationGuidanceRecoveryBoundary::SelectionWithStartedPlanner,
    )?;
    let session_path = fixture.session_path.clone();
    let task_id = fixture.task_id.clone();
    let executor_requests = Arc::new(Mutex::new(Vec::new()));
    let guidance_review_requests = Arc::new(AtomicUsize::new(0));
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter))
        .with_task_role_provider_builder(Arc::new(CapturingApplicationTaskRoleProviderBuilder {
            executor_requests: Arc::clone(&executor_requests),
            guidance_review_requests: Arc::clone(&guidance_review_requests),
        }));
    let prepared = prepare_application_task_continuation(
        ApplicationTaskContinuationRequest {
            config_path: fixture.config_path,
            launch_cwd: temp.path().to_path_buf(),
            session_path: fixture.session_path,
            session_attachment: None,
            expected_session_scope_id: fixture.session_scope_id,
            run_id: "run-application-guidance-started-selection-recovery".to_owned(),
            task_id: fixture.task_id,
            guidance: None,
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    )
    .await?;
    let (execution, _control) = prepared.into_parts();
    let mut handler = RecordingApplicationRunEvents::default();
    let mut approval_handler = AutoApproveHandler;

    let output = execution
        .execute(&mut handler, &mut approval_handler)
        .await?;

    assert_eq!(output.task_status, TaskRunStatus::Completed);
    assert_eq!(guidance_review_requests.load(Ordering::SeqCst), 1);
    assert_eq!(
        executor_requests
            .lock()
            .expect("executor request lock should not be poisoned")
            .len(),
        2
    );
    let reopened = Session::load_from_store(
        "deepseek",
        "deepseek-v4-flash",
        JsonlSessionStore::new(&session_path)?,
    )?;
    let task = reopened
        .task_state_projection()
        .tasks
        .get(&task_id)
        .cloned()
        .expect("recovered task remains projected");
    let first = task
        .participant_attempts
        .values()
        .find(|attempt| {
            attempt.purpose == sigil_kernel::TaskParticipantPurpose::Planner && attempt.ordinal == 1
        })
        .expect("crashed planner attempt remains auditable");
    assert_eq!(
        first.status,
        sigil_kernel::TaskParticipantAttemptStatus::Interrupted
    );
    assert!(
        first
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("interrupted by explicit continuation"))
    );
    let second = task
        .participant_attempts
        .values()
        .find(|attempt| {
            attempt.purpose == sigil_kernel::TaskParticipantPurpose::Planner && attempt.ordinal == 2
        })
        .expect("explicit retry uses a fresh planner attempt ordinal");
    assert_eq!(
        second.status,
        sigil_kernel::TaskParticipantAttemptStatus::Completed
    );
    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskGuidanceApplied(applied))
                    if applied.task_id == task_id
            ))
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn application_continuation_recovers_exact_selection_only_guidance_after_reload() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let fixture = application_guidance_recovery_fixture(
        temp.path(),
        true,
        ApplicationGuidanceRecoveryBoundary::SelectionOnly,
    )?;
    let session_path = fixture.session_path.clone();
    let exact_guidance = fixture.exact_guidance.clone();
    let executor_requests = Arc::new(Mutex::new(Vec::new()));
    let guidance_review_requests = Arc::new(AtomicUsize::new(0));
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter))
        .with_task_role_provider_builder(Arc::new(CapturingApplicationTaskRoleProviderBuilder {
            executor_requests: Arc::clone(&executor_requests),
            guidance_review_requests: Arc::clone(&guidance_review_requests),
        }));
    let prepared = prepare_application_task_continuation(
        ApplicationTaskContinuationRequest {
            config_path: fixture.config_path,
            launch_cwd: temp.path().to_path_buf(),
            session_path: fixture.session_path,
            session_attachment: None,
            expected_session_scope_id: fixture.session_scope_id,
            run_id: "run-application-guidance-exact-selection-recovery".to_owned(),
            task_id: fixture.task_id,
            guidance: None,
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    )
    .await?;
    let (execution, _control) = prepared.into_parts();
    let mut handler = RecordingApplicationRunEvents::default();
    let mut approval_handler = AutoApproveHandler;

    let output = execution
        .execute(&mut handler, &mut approval_handler)
        .await
        .expect("safe durable guidance projection should recover after reload");

    assert_eq!(output.task_status, TaskRunStatus::Completed);
    assert_eq!(guidance_review_requests.load(Ordering::SeqCst), 1);
    assert!(
        executor_requests
            .lock()
            .expect("executor request lock should not be poisoned")
            .iter()
            .all(|request| {
                request
                    .messages
                    .iter()
                    .filter_map(|message| message.content.as_deref())
                    .all(|content| !content.contains(&exact_guidance))
            })
    );
    let durable_log = std::fs::read_to_string(session_path)?;
    assert!(!durable_log.contains(&exact_guidance));
    assert!(durable_log.contains("task_guidance_applied"));
    Ok(())
}

#[test]
fn application_exact_reentry_preserves_active_task_then_recovers_started_planner() -> Result<()> {
    std::thread::Builder::new()
        .name("application-exact-reentry-recovery".to_owned())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| -> Result<()> {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    let temp = tempfile::tempdir()?;
                    let fixture = application_guidance_recovery_fixture(
                        temp.path(),
                        true,
                        ApplicationGuidanceRecoveryBoundary::SelectionWithStartedPlanner,
                    )?;
                    let config_path = fixture.config_path.clone();
                    let session_path = fixture.session_path.clone();
                    let session_scope_id = fixture.session_scope_id.clone();
                    let task_id = fixture.task_id.clone();
                    let exact_guidance = fixture.exact_guidance.clone();
                    let executor_requests = Arc::new(Mutex::new(Vec::new()));
                    let guidance_review_requests = Arc::new(AtomicUsize::new(0));
                    let services =
                        ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter))
                            .with_task_role_provider_builder(Arc::new(
                                CapturingApplicationTaskRoleProviderBuilder {
                                    executor_requests: Arc::clone(&executor_requests),
                                    guidance_review_requests: Arc::clone(&guidance_review_requests),
                                },
                            ));

                    let prepared = prepare_application_task_continuation(
                        ApplicationTaskContinuationRequest {
                            config_path: config_path.clone(),
                            launch_cwd: temp.path().to_path_buf(),
                            session_path: session_path.clone(),
                            session_attachment: None,
                            expected_session_scope_id: session_scope_id.clone(),
                            run_id: "run-application-guidance-started-missing-exact".to_owned(),
                            task_id: task_id.clone(),
                            guidance: None,
                            interaction: ApplicationRunInteraction::NonInteractive,
                            permission_mode: None,
                        },
                        &services,
                    )
                    .await?;
                    let (execution, first_control) = prepared.into_parts();
                    let mut handler = RecordingApplicationRunEvents::default();
                    let mut approval_handler = AutoApproveHandler;
                    let output = execution
                        .execute(&mut handler, &mut approval_handler)
                        .await
                        .expect(
                            "safe durable guidance projection should recover the started planner",
                        );
                    assert_eq!(output.task_status, TaskRunStatus::Completed);

                    let reopened = Session::load_from_store(
                        "deepseek",
                        "deepseek-v4-flash",
                        JsonlSessionStore::new(&session_path)?,
                    )?;
                    let task = reopened
                        .task_state_projection()
                        .tasks
                        .get(&task_id)
                        .cloned()
                        .expect("active Task remains projected after admission failure");
                    assert_eq!(task.status, TaskRunStatus::Completed);
                    assert_eq!(guidance_review_requests.load(Ordering::SeqCst), 1);
                    assert!(
                        executor_requests
                            .lock()
                            .expect("executor request lock should not be poisoned")
                            .iter()
                            .all(|request| {
                                request
                                    .messages
                                    .iter()
                                    .filter_map(|message| message.content.as_deref())
                                    .all(|content| !content.contains(&exact_guidance))
                            })
                    );
                    drop(first_control);

                    let reopened = Session::load_from_store(
                        "deepseek",
                        "deepseek-v4-flash",
                        JsonlSessionStore::new(&session_path)?,
                    )?;
                    let task = reopened
                        .task_state_projection()
                        .tasks
                        .get(&task_id)
                        .cloned()
                        .expect("recovered Task remains projected");
                    assert_eq!(task.status, TaskRunStatus::Completed);
                    assert_eq!(
                        task.participant_attempts
                            .values()
                            .find(|attempt| {
                                attempt.purpose == sigil_kernel::TaskParticipantPurpose::Planner
                                    && attempt.ordinal == 1
                            })
                            .map(|attempt| attempt.status),
                        Some(sigil_kernel::TaskParticipantAttemptStatus::Interrupted)
                    );
                    assert_eq!(
                        task.participant_attempts
                            .values()
                            .find(|attempt| {
                                attempt.purpose == sigil_kernel::TaskParticipantPurpose::Planner
                                    && attempt.ordinal == 2
                            })
                            .map(|attempt| attempt.status),
                        Some(sigil_kernel::TaskParticipantAttemptStatus::Completed)
                    );
                    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskContinuationSelected(selected))
                    if selected.task_id == task_id
            ))
            .count(),
        1
    );
                    assert!(!std::fs::read_to_string(session_path)?.contains(&exact_guidance));
                    Ok(())
                })
        })?
        .join()
        .map_err(|_| anyhow::anyhow!("application exact re-entry recovery thread panicked"))?
}

#[tokio::test]
async fn application_continuation_reenters_exact_selection_only_guidance_with_original_scope()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let fixture = application_guidance_recovery_fixture(
        temp.path(),
        true,
        ApplicationGuidanceRecoveryBoundary::SelectionOnly,
    )?;
    let session_path = fixture.session_path.clone();
    let task_id = fixture.task_id.clone();
    let exact_guidance = fixture.exact_guidance.clone();
    let executor_requests = Arc::new(Mutex::new(Vec::new()));
    let guidance_review_requests = Arc::new(AtomicUsize::new(0));
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter))
        .with_task_role_provider_builder(Arc::new(CapturingApplicationTaskRoleProviderBuilder {
            executor_requests: Arc::clone(&executor_requests),
            guidance_review_requests: Arc::clone(&guidance_review_requests),
        }));
    let prepared = prepare_application_task_continuation(
        ApplicationTaskContinuationRequest {
            config_path: fixture.config_path,
            launch_cwd: temp.path().to_path_buf(),
            session_path: fixture.session_path,
            session_attachment: None,
            expected_session_scope_id: fixture.session_scope_id,
            run_id: "run-application-guidance-exact-selection-reentry".to_owned(),
            task_id: fixture.task_id,
            guidance: Some(exact_guidance.clone()),
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    )
    .await?;
    let (execution, _control) = prepared.into_parts();
    let mut handler = RecordingApplicationRunEvents::default();
    let mut approval_handler = AutoApproveHandler;

    let output = execution
        .execute(&mut handler, &mut approval_handler)
        .await?;

    assert_eq!(output.task_status, TaskRunStatus::Completed);
    assert_eq!(guidance_review_requests.load(Ordering::SeqCst), 1);
    let prompts = executor_requests
        .lock()
        .expect("executor request lock should not be poisoned")
        .iter()
        .map(|request| {
            request
                .messages
                .iter()
                .filter_map(|message| message.content.as_deref())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect::<Vec<_>>();
    assert_eq!(prompts.len(), 2);
    let first = prompts
        .iter()
        .find(|prompt| prompt.contains("Step: step_1"))
        .expect("first executor step request");
    let second = prompts
        .iter()
        .find(|prompt| prompt.contains("Step: step_2"))
        .expect("second executor step request");
    assert!(!first.contains(&exact_guidance));
    assert!(second.contains(&exact_guidance));

    let reopened = Session::load_from_store(
        "deepseek",
        "deepseek-v4-flash",
        JsonlSessionStore::new(&session_path)?,
    )?;
    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskGuidanceApplied(applied))
                    if applied.task_id == task_id
                        && applied.target_step_ids == vec![TaskStepId::new("step_2")
                            .expect("valid recovery target")]
            ))
            .count(),
        1
    );
    assert!(!std::fs::read_to_string(session_path)?.contains(&exact_guidance));
    Ok(())
}

#[tokio::test]
async fn application_continuation_rejects_mismatched_exact_selection_before_provider_io()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let fixture = application_guidance_recovery_fixture(
        temp.path(),
        true,
        ApplicationGuidanceRecoveryBoundary::SelectionOnly,
    )?;
    let session_path = fixture.session_path.clone();
    let executor_requests = Arc::new(Mutex::new(Vec::new()));
    let guidance_review_requests = Arc::new(AtomicUsize::new(0));
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter))
        .with_task_role_provider_builder(Arc::new(CapturingApplicationTaskRoleProviderBuilder {
            executor_requests: Arc::clone(&executor_requests),
            guidance_review_requests: Arc::clone(&guidance_review_requests),
        }));
    let prepared = prepare_application_task_continuation(
        ApplicationTaskContinuationRequest {
            config_path: fixture.config_path,
            launch_cwd: temp.path().to_path_buf(),
            session_path: fixture.session_path,
            session_attachment: None,
            expected_session_scope_id: fixture.session_scope_id,
            run_id: "run-application-guidance-mismatched-selection-reentry".to_owned(),
            task_id: fixture.task_id,
            guidance: Some(
                "replace step 1 entirely with authorization=a-different-secret-value".to_owned(),
            ),
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    )
    .await?;
    let (execution, _control) = prepared.into_parts();
    let mut handler = RecordingApplicationRunEvents::default();
    let mut approval_handler = AutoApproveHandler;

    let error = execution
        .execute(&mut handler, &mut approval_handler)
        .await
        .expect_err("mismatched exact guidance must fail before provider I/O");

    assert!(
        format!("{error:#}")
            .contains("explicit guidance conflicts with pending durable task guidance")
    );
    assert_eq!(guidance_review_requests.load(Ordering::SeqCst), 0);
    assert!(
        executor_requests
            .lock()
            .expect("executor request lock should not be poisoned")
            .is_empty()
    );
    assert!(!std::fs::read_to_string(session_path)?.contains("task_guidance_applied"));
    Ok(())
}

#[tokio::test]
async fn typed_continuation_cannot_fork_unfinished_materialized_guidance() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let fixture = application_guidance_recovery_fixture(
        temp.path(),
        false,
        ApplicationGuidanceRecoveryBoundary::Materialized,
    )?;
    let root_config = RootConfig::load(&fixture.config_path)?;
    let (provider_name, route) =
        crate::provider_connections::resolve_default_model_route(&root_config)
            .map_err(anyhow::Error::new)?;
    let mut session = Session::load_from_store_with_route(
        provider_name,
        route.model_ref.model_id.clone(),
        Some(route),
        JsonlSessionStore::new(&fixture.session_path)?,
    )?;
    let source_turn = sigil_kernel::ConversationTurnRef::new(
        session.session_scope_id(),
        "message-materialized-guidance-new-selection",
        "run-materialized-guidance-new-selection",
    )?;
    let mut source_message = ModelMessage::user(fixture.safe_guidance.clone());
    source_message.id = source_turn.message_id.clone();
    session.append_user_message(source_message)?;
    session.append_control(ControlEntry::ConversationRouteDecisionRecorded(
        sigil_kernel::ConversationRouteDecisionRecordedEntry {
            decision_id: sigil_kernel::conversation_route_decision_id_for_source(&source_turn),
            source_turn: source_turn.clone(),
            route: sigil_kernel::ConversationRoute::Task,
            reason_codes: Vec::new(),
            configured_policy: TaskRoutingPolicy::Auto,
            effective_capability: sigil_kernel::AutomaticRouteCapability::DirectTask,
            policy_snapshot_hash: "task-routing-policy".to_owned(),
            route_contract_fingerprint: "sha256:new-selection-after-materialization".to_owned(),
            decided_at_ms: 2,
        },
    ))?;
    let prompt = sigil_kernel::project_conversation_prompt_for_persistence(&fixture.exact_guidance);
    let selection = sigil_kernel::TaskContinuationSelectedEntry {
        task_id: fixture.task_id.clone(),
        plan_version: Some(1),
        task_status: TaskRunStatus::Paused,
        plan_status: Some(TaskPlanStatus::Accepted),
        source_turn: source_turn.clone(),
        route_contract_fingerprint: "sha256:new-selection-after-materialization".to_owned(),
        control: sigil_kernel::TaskContinuationControlKind::ApplyCurrentRequestAsGuidance,
        prompt_hash: prompt.prompt_hash,
        exact_prompt_required: prompt.exact_prompt_required,
        guidance: prompt.safe_prompt,
        selected_at_ms: 2,
    };
    session.append_control(ControlEntry::TaskContinuationSelected(selection.clone()))?;
    let profile_registry =
        crate::AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &root_config,
            temp.path(),
            session.entries(),
        )?;
    let executor_requests = Arc::new(Mutex::new(Vec::new()));
    let guidance_review_requests = Arc::new(AtomicUsize::new(0));
    let task_execution = ApplicationTaskExecutionRuntime {
        root_config: root_config.clone(),
        workspace_root: temp.path().to_path_buf(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        options: crate::build_run_options(
            &root_config,
            temp.path().to_path_buf(),
            InteractionMode::Headless,
            None,
        ),
        base_registry: ToolRegistry::new(),
        agent_supervisor: crate::AgentSupervisor::new(
            profile_registry,
            crate::AgentBudgetPolicy::from_root_config(&root_config),
            application_task_provider_capabilities(),
        ),
        role_provider_builder: Arc::new(CapturingApplicationTaskRoleProviderBuilder {
            executor_requests: Arc::clone(&executor_requests),
            guidance_review_requests: Arc::clone(&guidance_review_requests),
        }),
    };
    let cancellation_owner = RunCancellationOwner::new();
    let cancellation_handle = cancellation_owner.handle();
    let root_output = AgentRunOutput {
        disposition: AgentRunDisposition::ContinueDurableTask(Box::new(
            sigil_kernel::ContinueDurableTaskAction {
                task_id: fixture.task_id.clone(),
                source_turn,
                plan_version: Some(1),
                task_status: TaskRunStatus::Paused,
                plan_status: Some(TaskPlanStatus::Accepted),
                route_contract_fingerprint: "sha256:new-selection-after-materialization".to_owned(),
                control: sigil_kernel::TaskContinuationControl::ApplyTaskGuidance(
                    fixture.exact_guidance.clone(),
                ),
                guidance: sigil_kernel::SecretString::new(fixture.exact_guidance),
                guidance_receipt: selection,
            },
        )),
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

    let error = continue_application_task_handoff(
        &mut session,
        root_output,
        Some(task_execution),
        &mut handler,
        &mut approval_handler,
        &cancellation_handle,
    )
    .await
    .expect_err("a new typed authority must not fork unfinished materialized guidance");

    assert!(
        format!("{error:#}").contains("conflicts with unfinished durable materialization"),
        "unexpected conflict error: {error:#}"
    );
    assert_eq!(guidance_review_requests.load(Ordering::SeqCst), 0);
    assert!(
        executor_requests
            .lock()
            .expect("executor request lock should not be poisoned")
            .is_empty()
    );
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskGuidanceApplied(applied))
                    if applied.task_id == fixture.task_id
            ))
            .count(),
        1,
        "the original materialization remains the only planner-owned decision"
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
            .any(|tool| tool.name == sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME)
        {
            let args = r#"{
                "schema_version": 2,
                "summary": "Migrate the coordinator",
                "steps": [{
                    "step_id": "migrate_1",
                    "title": "Migrate coordinator",
                    "role": "executor",
                    "mode": "write",
                    "isolation": "sequential_workspace_write",
                    "target_paths": ["src/coordinator.rs"]
                }],
                "target_paths": ["src/coordinator.rs"],
                "suggested_checks": ["cargo test"]
            }"#;
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "plan-draft-call".to_owned(),
                    name: sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "plan-draft-call".to_owned(),
                    delta: args.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "plan-draft-call".to_owned(),
                    name: sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned(),
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
    let prepared = prepare_application_run(request, &services).await?;
    let ApplicationRunExecutionKind::Main { input, .. } = &prepared.execution.kind else {
        panic!("ordinary application request must prepare the main agent");
    };
    let Some(AgentRunPurpose::Conversation(context)) = input.purpose.as_ref() else {
        panic!("conversation purpose expected");
    };
    let plan_review_binding = context.plan_review.clone().expect("plan review binding");
    let source_turn = context.source_turn.clone();
    drop(prepared);

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
    let mut session = Session::new("application-plan-review", "planned-model");
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
        root_config: root_config.clone(),
        agent: Box::new(sigil_kernel::Agent::new(
            Box::new(PlanReviewDraftProvider),
            sigil_kernel::ToolRegistry::new(),
        )),
        tool_registry: sigil_kernel::ToolRegistry::new(),
        workspace_snapshot_id: None,
    };
    let cancellation_owner = sigil_kernel::RunCancellationOwner::new();
    let mut handler = RecordingRunEvents::default();
    let mut approval_handler = sigil_kernel::AutoApproveHandler;
    let output = super::continue_application_plan_review(
        &mut session,
        agent_output,
        Some(runtime),
        &mut handler,
        &mut approval_handler,
        &cancellation_owner.handle(),
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
        .as_deref()
        .expect("plan-ready success must bind a durable final assistant");
    assert!(handler.0.iter().any(|event| matches!(
        event,
        RunEvent::Control(ControlEntry::PlanReviewAttempt(attempt))
            if attempt.status == sigil_kernel::PlanReviewAttemptStatus::DraftReady
    )));
    assert!(handler.0.iter().any(|event| matches!(
        event,
        RunEvent::AssistantMessage(message)
            if message.id == final_message_id
                && message.assistant_kind == Some(AssistantMessageKind::FinalAnswer)
    )));
    let plan_projection = session.plan_artifact_projection();
    let draft = plan_projection
        .plans
        .get(&plan_review_binding.plan_id)
        .expect("draft committed to parent");
    assert_eq!(draft.summary, "Migrate the coordinator");
    let review_projection = sigil_kernel::PlanReviewProjection::from_entries(session.entries());
    let attempt = review_projection
        .latest_attempt(&plan_review_binding.plan_review_id)
        .expect("attempt");
    assert_eq!(
        attempt.status,
        sigil_kernel::PlanReviewAttemptStatus::DraftReady
    );
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
    let request = crate::PlanReviewRunRequest {
        plan_review_id: sigil_kernel::PlanReviewId::new("review-pending-plan-route")?,
        attempt_id: sigil_kernel::PlanReviewAttemptId::new("attempt-pending-plan-route")?,
        plan_id: sigil_kernel::PlanId::new("plan_pending_route")?,
        source: sigil_kernel::PlanReviewSource::AutomaticConversationRoute,
        source_turn: sigil_kernel::ConversationTurnRef::new(
            session.session_scope_id(),
            "message-pending-plan-route",
            "run-pending-plan-route",
        )?,
        route_decision_id: None,
        child_session_ref: SessionRef::new_relative("child.jsonl")?,
        finalizer_session_ref: SessionRef::new_relative("finalizer.jsonl")?,
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        objective: "inspect the pending plan route".to_owned(),
        workspace_snapshot_id: base_snapshot.clone(),
    };
    let session_scope_id = session.session_scope_id().to_owned();
    crate::PlanReviewCoordinator::ensure_attempt_started(&mut session, &request, 1)?;
    let draft = sigil_kernel::plan_draft_created_entry_with_plan_id(
        request.plan_id.clone(),
        r#"```sigil-plan-v2
{"summary":"Inspect the pending plan route","steps":[{"step_id":"inspect","title":"Inspect","role":"executor","depends_on":[],"mode":"read","isolation":"shared_read_only","target_paths":["session.jsonl"]}]}
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
        &sigil_kernel::PlanCompileInputV1 {
            source_attempt_id: request.attempt_id.as_str().to_owned(),
            source_turn_id: request.source_turn.message_id.clone(),
            task_config_contract_hash: sigil_kernel::stable_event_uuid(
                "sigil-plan-task-config-v1",
                "test",
            ),
            planner_schema_hash: sigil_kernel::stable_event_uuid(
                "sigil-plan-planner-schema-v1",
                "v2",
            ),
            task_contract_schema_hash: sigil_kernel::stable_event_uuid(
                "sigil-task-contract-schema-v1",
                "v2",
            ),
            intent_schema_hash: None,
            max_plan_steps: 64,
            workspace_id: None,
            session_scope_id: Some(session_scope_id.clone()),
        },
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
        workspace_root: temp.path().to_path_buf(),
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
    };
    let cancellation_owner = RunCancellationOwner::new();
    let cancellation_handle = cancellation_owner.handle();
    let action = sigil_kernel::RunPendingPlanAction {
        plan_id: plan_id.clone(),
        plan_hash: plan_hash.clone(),
        source_turn: sigil_kernel::ConversationTurnRef::new(
            session.session_scope_id(),
            "message-pending-plan-route",
            "run-pending-plan-route",
        )?,
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
    // The single adoption authority exists; the task ran through admission to a terminal.
    let artifacts = session.plan_artifact_projection();
    let adoption = artifacts
        .adoptions
        .get(&plan_id)
        .and_then(|entries| entries.first())
        .expect("model route must adopt the plan");
    assert_eq!(adoption.plan_hash, plan_hash);
    let tasks = session.task_state_projection();
    assert!(
        tasks
            .admission_attempts
            .get(&adoption.task_id)
            .is_some_and(|attempts| !attempts.is_empty()),
        "model route must run admission before the runner"
    );
    assert_eq!(
        tasks.tasks.get(&adoption.task_id).map(|task| task.status),
        Some(TaskRunStatus::Completed)
    );
    assert!(output.result.final_message_id.is_some());
    Ok(())
}

#[tokio::test]
async fn run_pending_plan_route_keeps_blocked_task_durable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    // Config without any connection: the honest admission probes block with provider_unavailable
    // after adoption; the Task stays durable with a typed blocker.
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
max_plan_steps = 64
"#,
    )?;
    let root_config = RootConfig::load(&config_path)?;
    let session_path = temp.path().join(".sigil/sessions/session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session =
        Session::load_from_store("application-task-test", "application-task-model", store)?;
    let base_snapshot = crate::plan_handoff_workspace_snapshot_id(&root_config, temp.path())?;
    let request = crate::PlanReviewRunRequest {
        plan_review_id: sigil_kernel::PlanReviewId::new("review-pending-plan-blocked")?,
        attempt_id: sigil_kernel::PlanReviewAttemptId::new("attempt-pending-plan-blocked")?,
        plan_id: sigil_kernel::PlanId::new("plan_pending_blocked")?,
        source: sigil_kernel::PlanReviewSource::AutomaticConversationRoute,
        source_turn: sigil_kernel::ConversationTurnRef::new(
            session.session_scope_id(),
            "message-pending-plan-blocked",
            "run-pending-plan-blocked",
        )?,
        route_decision_id: None,
        child_session_ref: SessionRef::new_relative("child.jsonl")?,
        finalizer_session_ref: SessionRef::new_relative("finalizer.jsonl")?,
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        objective: "inspect the blocked pending plan route".to_owned(),
        workspace_snapshot_id: base_snapshot.clone(),
    };
    let session_scope_id = session.session_scope_id().to_owned();
    crate::PlanReviewCoordinator::ensure_attempt_started(&mut session, &request, 1)?;
    let draft = sigil_kernel::plan_draft_created_entry_with_plan_id(
        request.plan_id.clone(),
        r#"```sigil-plan-v2
{"summary":"Inspect the blocked route","steps":[{"step_id":"inspect","title":"Inspect","role":"executor","depends_on":[],"mode":"read","isolation":"shared_read_only","target_paths":["session.jsonl"]}]}
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
        &sigil_kernel::PlanCompileInputV1 {
            source_attempt_id: request.attempt_id.as_str().to_owned(),
            source_turn_id: request.source_turn.message_id.clone(),
            task_config_contract_hash: sigil_kernel::stable_event_uuid(
                "sigil-plan-task-config-v1",
                "test",
            ),
            planner_schema_hash: sigil_kernel::stable_event_uuid(
                "sigil-plan-planner-schema-v1",
                "v2",
            ),
            task_contract_schema_hash: sigil_kernel::stable_event_uuid(
                "sigil-task-contract-schema-v1",
                "v2",
            ),
            intent_schema_hash: None,
            max_plan_steps: 64,
            workspace_id: None,
            session_scope_id: Some(session_scope_id.clone()),
        },
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
        workspace_root: temp.path().to_path_buf(),
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
    };
    let cancellation_owner = RunCancellationOwner::new();
    let cancellation_handle = cancellation_owner.handle();
    let action = sigil_kernel::RunPendingPlanAction {
        plan_id: plan_id.clone(),
        plan_hash: plan_hash.clone(),
        source_turn: sigil_kernel::ConversationTurnRef::new(
            session.session_scope_id(),
            "message-pending-plan-blocked",
            "run-pending-plan-blocked",
        )?,
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
    assert!(
        output.result.final_text.contains("blocked"),
        "blocked run must surface the blocker: {}",
        output.result.final_text
    );
    let artifacts = session.plan_artifact_projection();
    let adoption = artifacts
        .adoptions
        .get(&plan_id)
        .and_then(|entries| entries.first())
        .expect("model route must adopt the plan before blocking");
    let tasks = session.task_state_projection();
    assert_eq!(
        tasks.execution_phase(&adoption.task_id),
        Some(sigil_kernel::TaskExecutionPhaseV1::Blocked)
    );
    assert_eq!(
        tasks
            .active_blocker(&adoption.task_id)
            .map(|blocker| blocker.reason_code),
        Some(sigil_kernel::TaskBlockerReasonCodeV1::ProviderUnavailable)
    );
    Ok(())
}

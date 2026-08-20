use std::{
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use futures::{Stream, stream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener as TokioTcpListener;

use sigil_kernel::{
    Agent, AgentRunOptions, AgentRunPurpose, AutoApproveHandler, CompactionConfig,
    CompletionRequest, ControlEntry, ConversationRoute, ConversationRouteDecisionRecordedEntry,
    ConversationTurnRef, EventHandler, InteractionMode, JsonlSessionStore, MemoryConfig,
    ModelMessage, NoopEventHandler, PermissionConfig, PermissionEvaluationContext, PlanDecision,
    PlanDraftCreatedEntry, PlanReviewAttemptStatus, PlanReviewProjection, PlanReviewSource,
    Provider, ProviderCapabilities, ProviderChunk, RunCancellationOwner, RunEvent, Session,
    SessionLogEntry, SessionRef, TaskRoutingPolicy, TaskRunStatus, Tool, ToolAccess, ToolCall,
    ToolCategory, ToolContext, ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta,
    ToolSpec, conversation_route_decision_id_for_source, plan_review_attempt_id_for_review,
    plan_review_plan_id_for_attempt, plan_review_policy_snapshot_hash,
};

use crate::PlanReviewRunOutcome;
use crate::{
    ConversationCoordinator, PlanDecisionCommand, PlanReviewCoordinator, PlanReviewRunRequest,
};

fn session_with_route_decision() -> Result<(Session, PlanReviewRunRequest)> {
    let mut session = Session::new("plan-review-test", "planned-model");
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "plan-review-test".to_owned(),
        model_name: "planned-model".to_owned(),
        resolved_model_route: None,
    })?;
    let prompt = "design the migration before touching anything";
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        "user-1".to_owned(),
        "plan-review-run",
    )?;
    let mut message = ModelMessage::user(prompt);
    message.id = "user-1".to_owned();
    session.append_user_message(message)?;
    let decision_id = conversation_route_decision_id_for_source(&source_turn);
    session.append_control(ControlEntry::ConversationRouteDecisionRecorded(
        ConversationRouteDecisionRecordedEntry {
            decision_id: decision_id.clone(),
            source_turn: source_turn.clone(),
            route: ConversationRoute::PlanReview,
            reason_codes: vec![sigil_kernel::ConversationRouteReason::ArchitecturalTradeoff],
            configured_policy: TaskRoutingPolicy::Auto,
            effective_capability: sigil_kernel::AutomaticRouteCapability::ReviewFirst,
            policy_snapshot_hash: plan_review_policy_snapshot_hash(),
            route_contract_fingerprint: "sha256:contract".to_owned(),
            decided_at_ms: 42,
        },
    ))?;
    let plan_review_id = sigil_kernel::plan_review_id_for_source(&source_turn);
    let attempt_id = plan_review_attempt_id_for_review(&plan_review_id);
    let plan_id = plan_review_plan_id_for_attempt(&plan_review_id, &attempt_id);
    let request = PlanReviewRunRequest {
        plan_review_id: plan_review_id.clone(),
        attempt_id: attempt_id.clone(),
        plan_id: plan_id.clone(),
        source: PlanReviewSource::AutomaticConversationRoute,
        source_turn,
        route_decision_id: Some(decision_id),
        child_session_ref: sigil_kernel::plan_review_child_session_ref(
            &plan_review_id,
            &attempt_id,
        ),
        finalizer_session_ref: sigil_kernel::plan_review_finalizer_session_ref(
            &plan_review_id,
            &attempt_id,
            1,
        ),
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        objective: prompt.to_owned(),
        workspace_snapshot_id: None,
    };
    Ok((session, request))
}

fn draft_entry(request: &PlanReviewRunRequest) -> PlanDraftCreatedEntry {
    PlanDraftCreatedEntry {
        plan_id: request.plan_id.clone(),
        schema_version: 2,
        source: request.plan_source_ref(),
        plan_hash: format!("sha256:{}", "d".repeat(64)),
        summary: "Migrate the coordinator".to_owned(),
        inline_text: None,
        steps: vec![sigil_kernel::PlanDraftStep {
            step_id: "step_1".to_owned(),
            title: "Move the coordinator".to_owned(),
            display_name: Some("coordinator".to_owned()),
            detail: None,
            role: Some(sigil_kernel::AgentRole::Executor),
            depends_on: Vec::new(),
            intent_aliases: Vec::new(),
            mode: Some(sigil_kernel::TaskStepMode::Write),
            isolation: Some(sigil_kernel::TaskIsolationMode::SequentialWorkspaceWrite),
            target_paths: vec!["src/coordinator.rs".to_owned()],
            required_capabilities: Vec::new(),
            deliverables: Vec::new(),
            acceptance_criteria: Vec::new(),
            suggested_checks: Vec::new(),
            risk: None,
            notes: Vec::new(),
        }],
        intent_proposal: None,
        target_paths: vec!["src/coordinator.rs".to_owned()],
        suggested_checks: Vec::new(),
        risk: None,
        notes: Vec::new(),
        workspace_snapshot_id: request.workspace_snapshot_id.clone(),
        created_at_ms: 50,
    }
}

fn test_plan_compile_input() -> sigil_kernel::PlanCompileInputV1 {
    sigil_kernel::PlanCompileInputV1 {
        source_attempt_id: "attempt-1".to_owned(),
        source_turn_id: "message-1".to_owned(),
        task_config_contract_hash: sigil_kernel::stable_event_uuid(
            "sigil-plan-task-config-v1",
            "test",
        ),
        planner_schema_hash: sigil_kernel::stable_event_uuid("sigil-plan-planner-schema-v1", "v2"),
        task_contract_schema_hash: sigil_kernel::stable_event_uuid(
            "sigil-task-contract-schema-v1",
            "v2",
        ),
        intent_schema_hash: Some(sigil_kernel::stable_event_uuid(
            "sigil-intent-schema-v1",
            "v1",
        )),
        max_plan_steps: 64,
        workspace_id: None,
        session_scope_id: Some("test-session".to_owned()),
    }
}

fn plan_review_provider_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        exact_prefix_cache: false,
        reports_cache_tokens: false,
        reasoning_stream: sigil_kernel::ReasoningStreamSupport::Unsupported,
        supports_reasoning_effort: false,
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
        supports_system_fingerprint: false,
        tool_name_max_chars: 64,
    }
}

fn plan_review_test_options(workspace_root: &std::path::Path) -> AgentRunOptions {
    AgentRunOptions {
        workspace_root: workspace_root.to_path_buf(),
        max_turns: None,
        tool_timeout_secs: 5,
        reasoning_effort: None,
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig::default(),
        permission_context: PermissionEvaluationContext::default(),
        permission_mode_override: None,
        memory_config: MemoryConfig::with_enabled(false),
        compaction_config: CompactionConfig::default(),
    }
}

fn submitted_draft_chunks(call_id: &str) -> Vec<Result<ProviderChunk>> {
    let args = r#"{
        "schema_version": 2,
        "summary": "Bounded plan review",
        "steps": [{
            "step_id": "step_1",
            "title": "Implement the bounded change",
            "role": "executor",
            "depends_on": [],
            "mode": "write",
            "isolation": "sequential_workspace_write",
            "target_paths": ["src/coordinator.rs"]
        }],
        "target_paths": ["src/coordinator.rs"],
        "suggested_checks": ["cargo test"]
    }"#;
    vec![
        Ok(ProviderChunk::ToolCallStart {
            id: call_id.to_owned(),
            name: sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned(),
        }),
        Ok(ProviderChunk::ToolCallArgsDelta {
            id: call_id.to_owned(),
            delta: args.to_owned(),
        }),
        Ok(ProviderChunk::ToolCallComplete(ToolCall {
            id: call_id.to_owned(),
            name: sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned(),
            args_json: args.to_owned(),
        })),
        Ok(ProviderChunk::Done),
    ]
}

fn invalid_draft_chunks(call_id: &str) -> Vec<Result<ProviderChunk>> {
    let args = r#"{"schema_version":2,"summary":"broken","steps":[}"#;
    vec![
        Ok(ProviderChunk::ToolCallStart {
            id: call_id.to_owned(),
            name: sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned(),
        }),
        Ok(ProviderChunk::ToolCallArgsDelta {
            id: call_id.to_owned(),
            delta: args.to_owned(),
        }),
        Ok(ProviderChunk::ToolCallComplete(ToolCall {
            id: call_id.to_owned(),
            name: sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned(),
            args_json: args.to_owned(),
        })),
        Ok(ProviderChunk::Done),
    ]
}

struct PlanReviewInspectionTool;

#[async_trait]
impl Tool for PlanReviewInspectionTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "inspect_workspace".to_owned(),
            description: "Returns one bounded read-only observation".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
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
            "inspect_workspace",
            "bounded evidence",
            ToolResultMeta::default(),
        ))
    }
}

#[derive(Clone, Default)]
struct LoopingPlanReviewProvider {
    request_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl Provider for LoopingPlanReviewProvider {
    fn name(&self) -> &str {
        "looping-plan-review"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        plan_review_provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_names = request
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        let request_index = {
            let mut requests = self
                .request_tools
                .lock()
                .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
            let request_index = requests.len();
            requests.push(tool_names.clone());
            request_index
        };
        if tool_names.iter().any(|name| name == "inspect_workspace") && request_index < 8 {
            let call_id = format!("inspect-{request_index}");
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: call_id.clone(),
                    name: "inspect_workspace".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: call_id.clone(),
                    delta: "{}".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: call_id,
                    name: "inspect_workspace".to_owned(),
                    args_json: "{}".to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }
        if tool_names
            .iter()
            .any(|name| name == sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME)
            && !tool_names.iter().any(|name| name == "inspect_workspace")
        {
            return Ok(Box::pin(stream::iter(submitted_draft_chunks(
                "bounded-draft",
            ))));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(
                "research did not converge".to_owned(),
            )),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[derive(Clone, Default)]
struct InterruptedPlanReviewProvider {
    request_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl Provider for InterruptedPlanReviewProvider {
    fn name(&self) -> &str {
        "interrupted-plan-review"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        plan_review_provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_names = request
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        let request_index = {
            let mut requests = self
                .request_tools
                .lock()
                .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
            let request_index = requests.len();
            requests.push(tool_names.clone());
            request_index
        };
        if request_index == 0 {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta(
                    "partial research response".to_owned(),
                )),
                Err(anyhow!("simulated TLS unexpected EOF")),
            ])));
        }
        assert!(
            tool_names
                .iter()
                .any(|name| name == sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME)
        );
        assert!(!tool_names.iter().any(|name| name == "inspect_workspace"));
        Ok(Box::pin(stream::iter(submitted_draft_chunks(
            "recovered-draft",
        ))))
    }
}

#[derive(Clone, Default)]
struct UncertainPlanReviewProvider {
    request_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[derive(Clone, Default)]
struct ViolatingFinalizerProvider {
    request_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[derive(Clone, Default)]
struct InvalidThenValidFinalizerProvider {
    request_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl Provider for InvalidThenValidFinalizerProvider {
    fn name(&self) -> &str {
        "invalid-then-valid-plan-finalizer"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        plan_review_provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let index = {
            let mut requests = self
                .request_tools
                .lock()
                .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
            let index = requests.len();
            requests.push(request.tools.iter().map(|tool| tool.name.clone()).collect());
            index
        };
        Ok(Box::pin(stream::iter(match index {
            0 => vec![
                Ok(ProviderChunk::TextDelta("research complete".to_owned())),
                Ok(ProviderChunk::Done),
            ],
            1 => invalid_draft_chunks("invalid-draft"),
            _ => submitted_draft_chunks("corrected-draft"),
        })))
    }
}

#[async_trait]
impl Provider for ViolatingFinalizerProvider {
    fn name(&self) -> &str {
        "violating-plan-finalizer"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        plan_review_provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let index = {
            let mut requests = self
                .request_tools
                .lock()
                .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
            let index = requests.len();
            requests.push(request.tools.iter().map(|tool| tool.name.clone()).collect());
            index
        };
        if index == 0 {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("research complete".to_owned())),
                Ok(ProviderChunk::Done),
            ])));
        }
        let call_id = format!("illegal-finalizer-call-{index}");
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: call_id.clone(),
                name: "inspect_workspace".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: call_id.clone(),
                delta: "{}".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: call_id,
                name: "inspect_workspace".to_owned(),
                args_json: "{}".to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for UncertainPlanReviewProvider {
    fn name(&self) -> &str {
        "uncertain-plan-review"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        plan_review_provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.request_tools
            .lock()
            .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?
            .push(request.tools.iter().map(|tool| tool.name.clone()).collect());
        Err(anyhow!("simulated uncertain transport outcome"))
    }
}

#[derive(Default)]
struct RecordingPlanReviewEvents(Vec<RunEvent>);

impl EventHandler for RecordingPlanReviewEvents {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        self.0.push(event);
        Ok(())
    }
}

#[derive(Default)]
struct AskingPlanReviewProvider {
    calls: AtomicUsize,
    request_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl Provider for AskingPlanReviewProvider {
    fn name(&self) -> &str {
        "asking-plan-review"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        plan_review_provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.request_tools
            .lock()
            .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?
            .push(request.tools.iter().map(|tool| tool.name.clone()).collect());
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if call == 0 {
            let args = r#"{
                "prompt": "Choose the migration boundary",
                "questions": [{
                    "id": "scope",
                    "header": "Scope",
                    "question": "Which module should be migrated first?",
                    "required": true,
                    "field": {"kind": "text", "multiline": false, "max_chars": 120}
                }]
            }"#;
            vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "ask-scope".to_owned(),
                    name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "ask-scope".to_owned(),
                    delta: args.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "ask-scope".to_owned(),
                    name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
                    args_json: args.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ]
        } else {
            submitted_draft_chunks("submit-after-answer")
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}

#[tokio::test]
async fn plan_review_research_question_resumes_the_same_attempt_from_its_child_session()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let (parent_session, request) = session_with_route_decision()?;
    let mut parent_session = parent_session.with_store(JsonlSessionStore::new(
        temp.path().join("sessions/session.jsonl"),
    )?);
    PlanReviewCoordinator::ensure_attempt_started(&mut parent_session, &request, 100)?;
    let provider = AskingPlanReviewProvider::default();
    let request_tools = Arc::clone(&provider.request_tools);
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut handler = NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let suspended = PlanReviewCoordinator::run_plan_review(
        &mut parent_session,
        &request,
        &agent,
        plan_review_test_options(temp.path()),
        ToolRegistry::new(),
        &mut handler,
        &mut approval_handler,
        RunCancellationOwner::new().handle(),
    )
    .await?;
    let PlanReviewRunOutcome::AwaitingUserInput { request: pending } = suspended else {
        panic!("plan review research must suspend for its durable question")
    };
    assert!(matches!(
        pending.source,
        sigil_kernel::UserInputSourceV1::PlanReviewResearch {
            ref plan_review_id,
            ref attempt_id,
        } if plan_review_id == &request.plan_review_id && attempt_id == &request.attempt_id
    ));
    PlanReviewCoordinator::close_plan_review_run(
        &mut parent_session,
        &request,
        &PlanReviewRunOutcome::AwaitingUserInput {
            request: pending.clone(),
        },
        110,
    )?;
    let waiting = PlanReviewProjection::from_entries(parent_session.entries())
        .latest_attempt(&request.plan_review_id)
        .cloned()
        .context("waiting attempt missing")?;
    assert_eq!(waiting.status, PlanReviewAttemptStatus::WaitingForInput);
    assert_eq!(
        waiting.pending_user_input.as_deref(),
        Some(pending.as_ref())
    );

    let (receipt, resumed) = PlanReviewCoordinator::accept_plan_review_research_input(
        &mut parent_session,
        sigil_kernel::UserInputDecisionCommandV1 {
            identity: pending.identity.clone(),
            request_hash: pending.request_hash.clone(),
            command_id: sigil_kernel::UserInputCommandId::new("answer-plan-research")?,
            decision: sigil_kernel::UserInputDecisionV1::Submitted {
                answers: vec![sigil_kernel::UserInputAnswerV1 {
                    question_id: "scope".to_owned(),
                    value: sigil_kernel::UserInputAnswerValueV1::Text {
                        value: "crates/sigil-kernel".to_owned(),
                    },
                }],
            },
        },
        120,
    )?;
    assert!(receipt.continuation_required);
    let resumed = resumed.context("submitted research answer must resume the attempt")?;
    assert_eq!(resumed.attempt_id, request.attempt_id);
    PlanReviewCoordinator::ensure_attempt_started(&mut parent_session, &resumed, 130)?;
    let completed = PlanReviewCoordinator::run_plan_review(
        &mut parent_session,
        &resumed,
        &agent,
        plan_review_test_options(temp.path()),
        ToolRegistry::new(),
        &mut handler,
        &mut approval_handler,
        RunCancellationOwner::new().handle(),
    )
    .await?;
    let PlanReviewRunOutcome::DraftReady { draft } = completed else {
        panic!("answered research question must continue to a draft")
    };
    PlanReviewCoordinator::commit_draft_from_child(
        &mut parent_session,
        &draft,
        &resumed,
        &test_plan_compile_input(),
        140,
    )?;
    assert_eq!(
        PlanReviewProjection::from_entries(parent_session.entries())
            .latest_attempt(&request.plan_review_id)
            .map(|attempt| attempt.status),
        Some(PlanReviewAttemptStatus::DraftReady)
    );
    let requests = request_tools
        .lock()
        .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
    assert!(
        requests[0]
            .iter()
            .any(|tool| tool == sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME)
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_research_is_bounded_and_finalizes_with_only_the_submit_tool() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (parent_session, request) = session_with_route_decision()?;
    let mut parent_session = parent_session.with_store(JsonlSessionStore::new(
        temp.path().join("sessions/session.jsonl"),
    )?);
    let provider = LoopingPlanReviewProvider::default();
    let request_tools = Arc::clone(&provider.request_tools);
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(PlanReviewInspectionTool));
    let owner = RunCancellationOwner::new();
    let mut handler = NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let outcome = PlanReviewCoordinator::run_plan_review(
        &mut parent_session,
        &request,
        &agent,
        plan_review_test_options(temp.path()),
        registry,
        &mut handler,
        &mut approval_handler,
        owner.handle(),
    )
    .await?;

    assert!(matches!(outcome, PlanReviewRunOutcome::DraftReady { .. }));
    assert!(
        owner.handle().is_naturally_finalized(),
        "the coordinator must claim the root terminal only after its internal phases complete"
    );
    let requests = request_tools
        .lock()
        .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
    assert_eq!(
        requests.len(),
        5,
        "four research turns must be followed by exactly one finalization turn"
    );
    assert!(requests[..4].iter().all(|tools| {
        tools.iter().any(|name| name == "inspect_workspace")
            && tools
                .iter()
                .any(|name| name == sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME)
    }));
    assert_eq!(
        requests[4],
        vec![sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned()],
        "the finalization turn must not retain research or hosted-tool preparation"
    );
    Ok(())
}

#[tokio::test]
async fn plan_revision_submit_only_non_submit_is_never_dispatched_and_closes_typed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (parent_session, request) = session_with_route_decision()?;
    let mut parent_session = parent_session.with_store(JsonlSessionStore::new(
        temp.path().join("sessions/session.jsonl"),
    )?);
    let provider = ViolatingFinalizerProvider::default();
    let request_tools = Arc::clone(&provider.request_tools);
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(PlanReviewInspectionTool));
    let mut handler = RecordingPlanReviewEvents::default();
    let mut approval_handler = AutoApproveHandler;
    let outcome = PlanReviewCoordinator::run_plan_review(
        &mut parent_session,
        &request,
        &agent,
        plan_review_test_options(temp.path()),
        registry,
        &mut handler,
        &mut approval_handler,
        RunCancellationOwner::new().handle(),
    )
    .await?;
    assert!(matches!(
        outcome,
        PlanReviewRunOutcome::SubmitOnlyProtocolViolation(_)
    ));
    let requests = request_tools
        .lock()
        .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
    assert_eq!(requests.len(), 3, "research plus one corrective finalizer");
    assert!(requests[0].iter().any(|tool| tool == "inspect_workspace"));
    assert!(
        requests[1..]
            .iter()
            .all(|tools| { tools == &[sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned()] })
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_invalid_typed_finalizer_retries_once_in_a_fresh_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (parent_session, request) = session_with_route_decision()?;
    let mut parent_session = parent_session.with_store(JsonlSessionStore::new(
        temp.path().join("sessions/session.jsonl"),
    )?);
    let provider = InvalidThenValidFinalizerProvider::default();
    let request_tools = Arc::clone(&provider.request_tools);
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut handler = RecordingPlanReviewEvents::default();
    let mut approval_handler = AutoApproveHandler;

    let outcome = PlanReviewCoordinator::run_plan_review(
        &mut parent_session,
        &request,
        &agent,
        plan_review_test_options(temp.path()),
        ToolRegistry::new(),
        &mut handler,
        &mut approval_handler,
        RunCancellationOwner::new().handle(),
    )
    .await?;

    assert!(matches!(outcome, PlanReviewRunOutcome::DraftReady { .. }));
    let requests = request_tools
        .lock()
        .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
    assert_eq!(requests.len(), 3, "research plus two submit-only attempts");
    assert!(
        requests[1..]
            .iter()
            .all(|tools| tools == &[sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned()])
    );
    assert!(handler.0.iter().any(|event| matches!(
        event,
        RunEvent::Notice(message) if message.contains("invalid typed draft")
    )));
    Ok(())
}

#[tokio::test]
async fn plan_review_stream_interruption_uses_durable_evidence_for_submit_only_finalization()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let (parent_session, request) = session_with_route_decision()?;
    let mut parent_session = parent_session.with_store(JsonlSessionStore::new(
        temp.path().join("sessions/session.jsonl"),
    )?);
    let provider = InterruptedPlanReviewProvider::default();
    let request_tools = Arc::clone(&provider.request_tools);
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(PlanReviewInspectionTool));
    let owner = RunCancellationOwner::new();
    let mut handler = RecordingPlanReviewEvents::default();
    let mut approval_handler = AutoApproveHandler;

    let outcome = PlanReviewCoordinator::run_plan_review(
        &mut parent_session,
        &request,
        &agent,
        plan_review_test_options(temp.path()),
        registry,
        &mut handler,
        &mut approval_handler,
        owner.handle(),
    )
    .await?;

    assert!(matches!(outcome, PlanReviewRunOutcome::DraftReady { .. }));
    assert!(
        owner.handle().is_naturally_finalized(),
        "submit-only recovery must finalize the root cancellation tree exactly once"
    );
    let requests = request_tools
        .lock()
        .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
    assert_eq!(requests.len(), 2, "the failed request must not be replayed");
    assert!(requests[0].iter().any(|name| name == "inspect_workspace"));
    assert_eq!(
        requests[1],
        vec![sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned()]
    );
    assert!(handler.0.iter().any(|event| matches!(
        event,
        RunEvent::Notice(message) if message.contains("submit-only finalization")
    )));
    Ok(())
}

#[tokio::test]
async fn plan_review_transport_uncertainty_is_not_automatically_replayed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (parent_session, request) = session_with_route_decision()?;
    let mut parent_session = parent_session.with_store(JsonlSessionStore::new(
        temp.path().join("sessions/session.jsonl"),
    )?);
    let provider = UncertainPlanReviewProvider::default();
    let request_tools = Arc::clone(&provider.request_tools);
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(PlanReviewInspectionTool));
    let owner = RunCancellationOwner::new();
    let mut handler = NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let error = PlanReviewCoordinator::run_plan_review(
        &mut parent_session,
        &request,
        &agent,
        plan_review_test_options(temp.path()),
        registry,
        &mut handler,
        &mut approval_handler,
        owner.handle(),
    )
    .await
    .expect_err("an uncertain request must require explicit recovery");

    assert!(format!("{error:#}").contains("uncertain transport outcome"));
    assert_eq!(
        request_tools
            .lock()
            .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?
            .len(),
        1,
        "transport uncertainty must not trigger an automatic submit-only request"
    );
    Ok(())
}

#[test]
fn cancelled_and_failed_runs_close_the_durable_attempt_terminal() -> Result<()> {
    let (mut session, request) = session_with_route_decision()?;
    let action = sigil_kernel::StartPlanReviewAction {
        decision_id: request.route_decision_id.clone().expect("decision id"),
        plan_review_id: request.plan_review_id.clone(),
        plan_id: request.plan_id.clone(),
        source_turn: request.source_turn.clone(),
    };
    PlanReviewCoordinator::prepare_automatic_plan_review(&mut session, &action, None, 100)?;

    PlanReviewCoordinator::close_plan_review_run(
        &mut session,
        &request,
        &PlanReviewRunOutcome::Cancelled,
        120,
    )?;
    let projection = PlanReviewProjection::from_entries(session.entries());
    let attempt = projection
        .latest_attempt(&request.plan_review_id)
        .expect("attempt");
    assert_eq!(attempt.status, PlanReviewAttemptStatus::Cancelled);
    assert_eq!(
        attempt.terminal_reason,
        Some(sigil_kernel::PlanReviewTerminalReason::UserCancelled)
    );

    // A second lifecycle (revision-style identity) fails terminal and records `Failed`.
    let (mut failed_session, failed_request) = session_with_route_decision()?;
    let failed_action = sigil_kernel::StartPlanReviewAction {
        decision_id: failed_request
            .route_decision_id
            .clone()
            .expect("decision id"),
        plan_review_id: failed_request.plan_review_id.clone(),
        plan_id: failed_request.plan_id.clone(),
        source_turn: failed_request.source_turn.clone(),
    };
    PlanReviewCoordinator::prepare_automatic_plan_review(
        &mut failed_session,
        &failed_action,
        None,
        100,
    )?;
    PlanReviewCoordinator::close_plan_review_run(
        &mut failed_session,
        &failed_request,
        &PlanReviewRunOutcome::Failed("provider rejected the request".to_owned()),
        120,
    )?;
    let failed_projection = PlanReviewProjection::from_entries(failed_session.entries());
    let failed_attempt = failed_projection
        .latest_attempt(&failed_request.plan_review_id)
        .expect("attempt");
    assert_eq!(failed_attempt.status, PlanReviewAttemptStatus::Failed);
    assert_eq!(
        failed_attempt.terminal_reason,
        Some(sigil_kernel::PlanReviewTerminalReason::RunFailed)
    );

    // An interrupted run closes the same attempt with `Interrupted` / `RunInterrupted`.
    let (mut interrupted_session, interrupted_request) = session_with_route_decision()?;
    let interrupted_action = sigil_kernel::StartPlanReviewAction {
        decision_id: interrupted_request
            .route_decision_id
            .clone()
            .expect("decision id"),
        plan_review_id: interrupted_request.plan_review_id.clone(),
        plan_id: interrupted_request.plan_id.clone(),
        source_turn: interrupted_request.source_turn.clone(),
    };
    PlanReviewCoordinator::prepare_automatic_plan_review(
        &mut interrupted_session,
        &interrupted_action,
        None,
        100,
    )?;
    PlanReviewCoordinator::close_plan_review_run(
        &mut interrupted_session,
        &interrupted_request,
        &PlanReviewRunOutcome::Interrupted("run interrupted before a draft".to_owned()),
        120,
    )?;
    let interrupted_projection = PlanReviewProjection::from_entries(interrupted_session.entries());
    let interrupted_attempt = interrupted_projection
        .latest_attempt(&interrupted_request.plan_review_id)
        .expect("attempt");
    assert_eq!(
        interrupted_attempt.status,
        PlanReviewAttemptStatus::Interrupted
    );
    assert_eq!(
        interrupted_attempt.terminal_reason,
        Some(sigil_kernel::PlanReviewTerminalReason::RunInterrupted)
    );

    // close_plan_review_run_if_open is a no-op once the attempt is terminal.
    PlanReviewCoordinator::close_plan_review_run_if_open(
        &mut interrupted_session,
        &interrupted_request,
        &PlanReviewRunOutcome::Failed("late failure".to_owned()),
        130,
    )?;
    let reopened = PlanReviewProjection::from_entries(interrupted_session.entries());
    assert_eq!(
        reopened
            .latest_attempt(&interrupted_request.plan_review_id)
            .expect("attempt")
            .status,
        PlanReviewAttemptStatus::Interrupted
    );

    // close_plan_review_run_if_open is a no-op when the attempt was never started.
    let (mut fresh_session, fresh_request) = session_with_route_decision()?;
    PlanReviewCoordinator::close_plan_review_run_if_open(
        &mut fresh_session,
        &fresh_request,
        &PlanReviewRunOutcome::Failed("before start".to_owned()),
        130,
    )?;
    assert!(fresh_session.entries().iter().all(|entry| !matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(_))
    )));
    Ok(())
}

struct RecordingRevisionHandler(Vec<sigil_kernel::PublicRunEvent>);

impl crate::application_run::ApplicationRunEventHandler for RecordingRevisionHandler {
    fn handle_public_event(&mut self, event: sigil_kernel::PublicRunEvent) -> anyhow::Result<()> {
        self.0.push(event);
        Ok(())
    }
}

/// Persists a session with an accepted draft bound to the current workspace snapshot, records
/// the `Revise` decision, and returns the prepared revision run request.
fn seed_revision_decision(
    root_config: &sigil_kernel::RootConfig,
    workspace_root: &std::path::Path,
    session_path: &std::path::Path,
) -> Result<PlanReviewRunRequest> {
    let store = sigil_kernel::JsonlSessionStore::new(session_path)?;
    let (_, fallback_route) =
        crate::provider_connections::resolve_default_model_route(root_config)?;
    let mut session =
        crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
            root_config,
            &fallback_route,
            store,
            None,
            None,
            None,
        )?;
    let source =
        sigil_kernel::ConversationTurnRef::new(session.session_scope_id(), "message-1", "run-1")?;
    let mut user_message = sigil_kernel::ModelMessage::user("design the coordinator migration");
    user_message.id = "message-1".to_owned();
    session.append_user_message(user_message)?;
    let decision = sigil_kernel::ConversationRouteDecisionRecordedEntry {
        decision_id: sigil_kernel::ConversationRouteDecisionId::new("decision-revision")?,
        source_turn: source.clone(),
        route: sigil_kernel::ConversationRoute::PlanReview,
        reason_codes: vec![sigil_kernel::ConversationRouteReason::ArchitecturalTradeoff],
        configured_policy: sigil_kernel::TaskRoutingPolicy::Auto,
        effective_capability: sigil_kernel::AutomaticRouteCapability::ReviewFirst,
        policy_snapshot_hash: format!("sha256:{}", "a".repeat(64)),
        route_contract_fingerprint: format!("sha256:{}", "b".repeat(64)),
        decided_at_ms: 1,
    };
    let review_id = sigil_kernel::plan_review_id_for_source(&source);
    let decision_id = decision.decision_id.clone();
    session
        .append_control(sigil_kernel::ControlEntry::ConversationRouteDecisionRecorded(decision))?;
    let action = sigil_kernel::StartPlanReviewAction {
        decision_id,
        plan_review_id: review_id.clone(),
        plan_id: sigil_kernel::plan_review_plan_id_for_attempt(
            &review_id,
            &sigil_kernel::plan_review_attempt_id_for_review(&review_id),
        ),
        source_turn: source,
    };
    let workspace_snapshot_id =
        crate::plan_handoff_workspace_snapshot_id(root_config, workspace_root)?;
    let request = PlanReviewCoordinator::prepare_automatic_plan_review(
        &mut session,
        &action,
        workspace_snapshot_id,
        100,
    )?;
    let mut draft = draft_entry(&request);
    draft.steps = vec![sigil_kernel::PlanDraftStep {
        step_id: "migrate_1".to_owned(),
        title: "Migrate coordinator".to_owned(),
        display_name: None,
        detail: None,
        role: Some(sigil_kernel::AgentRole::Executor),
        depends_on: Vec::new(),
        intent_aliases: Vec::new(),
        mode: Some(sigil_kernel::TaskStepMode::Write),
        isolation: Some(sigil_kernel::TaskIsolationMode::SequentialWorkspaceWrite),
        target_paths: vec!["src/coordinator.rs".to_owned()],
        required_capabilities: Vec::new(),
        deliverables: Vec::new(),
        acceptance_criteria: Vec::new(),
        suggested_checks: Vec::new(),
        risk: None,
        notes: Vec::new(),
    }];
    PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        110,
    )?;
    let session_scope_id = session.session_scope_id().to_owned();
    drop(session);

    let receipt = crate::application_plan_decision(
        root_config,
        workspace_root,
        session_path,
        &session_scope_id,
        &crate::ApplicationPlanDecisionCommand {
            plan_id: request.plan_id.as_str().to_owned(),
            expected_plan_hash: draft.plan_hash.clone(),
            action: crate::ApplicationPlanAction::Revise,
            permission_grant: None,
        },
    )?;
    assert!(receipt.revision_request.is_none());
    let requested = receipt
        .user_input_request
        .expect("Revise must first create durable revision guidance");
    let (_, revision_request) = crate::application_plan_revision_guidance_decision(
        root_config,
        workspace_root,
        session_path,
        &session_scope_id,
        sigil_kernel::UserInputDecisionCommandV1 {
            identity: requested.identity,
            request_hash: requested.request_hash,
            command_id: sigil_kernel::UserInputCommandId::new("revision-guidance-command")?,
            decision: sigil_kernel::UserInputDecisionV1::Submitted {
                answers: vec![sigil_kernel::UserInputAnswerV1 {
                    question_id: "revision_guidance".to_owned(),
                    value: sigil_kernel::UserInputAnswerValueV1::Text {
                        value: "Preserve the existing compatibility boundary.".to_owned(),
                    },
                }],
            },
        },
    )?;
    revision_request.context("submitted revision guidance must prepare a revision run")
}

fn session_with_ready_plan() -> Result<(Session, PlanReviewRunRequest, PlanDraftCreatedEntry)> {
    let (mut session, request) = session_with_route_decision()?;
    PlanReviewCoordinator::ensure_attempt_started(&mut session, &request, 10)?;
    let draft = draft_entry(&request);
    PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        11,
    )?;
    Ok((session, request, draft))
}

fn submit_revision_guidance(
    session: &mut Session,
    base: &PlanDraftCreatedEntry,
) -> Result<PlanReviewRunRequest> {
    let requested = PlanReviewCoordinator::request_plan_revision_guidance(
        session,
        &base.plan_id,
        &base.plan_hash,
        20,
    )?;
    let (_, revision) = PlanReviewCoordinator::accept_plan_revision_guidance(
        session,
        sigil_kernel::UserInputDecisionCommandV1 {
            identity: requested.request.identity,
            request_hash: requested.request_hash,
            command_id: sigil_kernel::UserInputCommandId::new("revision-guidance-test-command")?,
            decision: sigil_kernel::UserInputDecisionV1::Submitted {
                answers: vec![sigil_kernel::UserInputAnswerV1 {
                    question_id: "revision_guidance".to_owned(),
                    value: sigil_kernel::UserInputAnswerValueV1::Text {
                        value: "Keep the public contract stable and split migration steps."
                            .to_owned(),
                    },
                }],
            },
        },
        Some("snapshot-revision".to_owned()),
        21,
    )?;
    revision.context("submitted revision guidance did not create an attempt")
}

fn public_plan_review_from_session(session: &Session) -> Result<sigil_kernel::PublicPlanReview> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    for entry in session.entries() {
        store.append_session_entry_event(entry)?;
    }
    let reloaded =
        Session::load_from_store(session.provider_name(), session.model_name(), store.clone())?;
    crate::conversation_display::conversation_display_page(
        store.path(),
        reloaded.session_scope_id(),
        None,
        20,
        None,
    )?
    .plan_review
    .context("public plan review is missing")
}

#[test]
fn plan_revision_guidance_is_durable_before_dispatch_and_retry_uses_a_fresh_attempt() -> Result<()>
{
    let (mut session, _base_request, base) = session_with_ready_plan()?;
    let requested = PlanReviewCoordinator::request_plan_revision_guidance(
        &mut session,
        &base.plan_id,
        &base.plan_hash,
        20,
    )?;
    assert!(
        session
            .plan_artifact_projection()
            .latest_decision(&base.plan_id)
            .is_none(),
        "opening Revise must not append RevisionRequested"
    );
    assert_eq!(
        session
            .user_input_projection()?
            .request(&requested.request.identity)
            .expect("guidance request")
            .status,
        sigil_kernel::UserInputStatusV1::Requested
    );

    let guidance_command = sigil_kernel::UserInputDecisionCommandV1 {
        identity: requested.request.identity.clone(),
        request_hash: requested.request_hash.clone(),
        command_id: sigil_kernel::UserInputCommandId::new("revision-guidance-first")?,
        decision: sigil_kernel::UserInputDecisionV1::Submitted {
            answers: vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "revision_guidance".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "Preserve compatibility.".to_owned(),
                },
            }],
        },
    };
    let (_, first) = PlanReviewCoordinator::accept_plan_revision_guidance(
        &mut session,
        guidance_command.clone(),
        None,
        21,
    )?;
    let first = first.expect("first revision attempt");
    assert_eq!(first.attempt_ordinal, 1);
    assert_eq!(
        first.revision_request_id.as_ref(),
        Some(&requested.request.identity.request_id)
    );
    assert_eq!(
        session
            .plan_artifact_projection()
            .latest_decision(&base.plan_id)
            .expect("revision decision")
            .decision,
        PlanDecision::RevisionRequested
    );

    let (replayed, recovered_before_spawn) = PlanReviewCoordinator::accept_plan_revision_guidance(
        &mut session,
        guidance_command,
        None,
        22,
    )?;
    assert!(replayed.idempotent_replay);
    assert_eq!(
        recovered_before_spawn.expect("durable accepted guidance must recover run authority"),
        first,
        "pre-spawn replay must recover the same physical revision attempt"
    );

    PlanReviewCoordinator::ensure_attempt_started(&mut session, &first, 23)?;
    PlanReviewCoordinator::close_plan_review_run(
        &mut session,
        &first,
        &PlanReviewRunOutcome::Failed("provider failed".to_owned()),
        24,
    )?;
    assert_eq!(
        session
            .plan_artifact_projection()
            .latest_decision(&base.plan_id)
            .expect("revision recovery")
            .decision,
        PlanDecision::RevisionFailed
    );

    let retry = PlanReviewCoordinator::retry_plan_revision(
        &mut session,
        &base.plan_id,
        &base.plan_hash,
        None,
        25,
    )?
    .expect("retry request");
    assert_eq!(retry.attempt_ordinal, 2);
    assert_ne!(retry.attempt_id, first.attempt_id);
    assert_eq!(retry.revision_request_id, first.revision_request_id);
    PlanReviewCoordinator::ensure_attempt_started(&mut session, &retry, 26)?;
    Ok(())
}

#[test]
fn plan_revision_every_terminal_failure_restores_the_base_plan() -> Result<()> {
    let outcomes = [
        PlanReviewRunOutcome::Failed("failed".to_owned()),
        PlanReviewRunOutcome::Interrupted("interrupted".to_owned()),
        PlanReviewRunOutcome::Cancelled,
        PlanReviewRunOutcome::SubmitOnlyProtocolViolation("wrong tool".to_owned()),
        PlanReviewRunOutcome::CompletedWithoutDraft,
    ];
    for (index, outcome) in outcomes.into_iter().enumerate() {
        let (mut session, _base_request, base) = session_with_ready_plan()?;
        let revision = submit_revision_guidance(&mut session, &base)?;
        PlanReviewCoordinator::ensure_attempt_started(&mut session, &revision, 30 + index as u64)?;
        match outcome {
            PlanReviewRunOutcome::CompletedWithoutDraft => {
                PlanReviewCoordinator::complete_without_draft(
                    &mut session,
                    &revision,
                    40 + index as u64,
                )?;
            }
            other => PlanReviewCoordinator::close_plan_review_run(
                &mut session,
                &revision,
                &other,
                40 + index as u64,
            )?,
        }
        let plan_projection = session.plan_artifact_projection();
        assert_eq!(
            plan_projection
                .latest_decision(&base.plan_id)
                .expect("terminal revision must settle the base plan")
                .decision,
            PlanDecision::RevisionFailed
        );
        assert!(plan_projection.plans.contains_key(&base.plan_id));
        let public = public_plan_review_from_session(&session)?;
        assert_eq!(public.plan_id, base.plan_id.as_str());
        assert_eq!(
            public.allowed_actions,
            vec![
                sigil_kernel::PublicPlanAction::Run,
                sigil_kernel::PublicPlanAction::Save,
                sigil_kernel::PublicPlanAction::Revise,
                sigil_kernel::PublicPlanAction::Reject,
            ],
            "terminal revision branch {index} must restore all base actions"
        );
        assert!(public.revision.is_some());
    }
    Ok(())
}

#[test]
fn plan_revision_success_switches_lineage_only_with_the_revised_draft() -> Result<()> {
    let (mut session, _base_request, base) = session_with_ready_plan()?;
    let revision = submit_revision_guidance(&mut session, &base)?;
    PlanReviewCoordinator::ensure_attempt_started(&mut session, &revision, 30)?;
    let mut revised = draft_entry(&revision);
    revised.summary = "Revised migration".to_owned();
    PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &revised,
        &revision,
        &test_plan_compile_input(),
        31,
    )?;
    assert_eq!(
        session
            .plan_artifact_projection()
            .latest_decision(&base.plan_id)
            .expect("revision success decision")
            .decision,
        PlanDecision::RevisionSucceeded
    );
    assert_eq!(
        PlanReviewProjection::from_entries(session.entries())
            .latest_attempt(&revision.plan_review_id)
            .expect("revised attempt")
            .plan_id,
        revised.plan_id
    );
    Ok(())
}

/// Local chat-completions SSE fixture that answers the revision plan review with a typed draft.
async fn spawn_revision_draft_fixture() -> Result<(tokio::task::JoinHandle<()>, String)> {
    let listener = TokioTcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let fixture = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut buffer = vec![0; 16384];
            let read = socket.read(&mut buffer).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let body = if request.contains("\"submit_plan_draft\"") {
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"revision-draft-call\",\"type\":\"function\",\"function\":{\"name\":\"submit_plan_draft\",\"arguments\":\"{\\\"schema_version\\\":2,\\\"summary\\\":\\\"Revised coordinator migration\\\",\\\"steps\\\":[{\\\"step_id\\\":\\\"migrate_2\\\",\\\"title\\\":\\\"Revise migration\\\",\\\"role\\\":\\\"executor\\\",\\\"mode\\\":\\\"write\\\",\\\"isolation\\\":\\\"sequential_workspace_write\\\",\\\"target_paths\\\":[\\\"src/coordinator.rs\\\"]}],\\\"target_paths\\\":[\\\"src/coordinator.rs\\\"],\\\"suggested_checks\\\":[\\\"cargo test\\\"]}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                    "data: [DONE]\n\n"
                )
            } else {
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"revised\"},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: [DONE]\n\n"
                )
            };
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
        }
    });
    Ok((fixture, format!("http://{address}")))
}

#[tokio::test]
async fn execute_plan_review_revision_runs_the_new_attempt_and_commits_the_draft() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let session_path = temp.path().join("session.jsonl");

    let (fixture, base_url) = spawn_revision_draft_fixture().await?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "local-test"
model = "gpt-4.1"
tool_timeout_secs = 5

[task]
routing_policy = "auto"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "{base_url}"
credential = {{ source = "none" }}
"#
        ),
    )?;
    let root_config: sigil_kernel::RootConfig =
        toml::from_str(&std::fs::read_to_string(&config_path)?)?;
    let workspace_root = sigil_kernel::resolve_workspace_root(&config_path, &workspace, ".");
    let revision_request = seed_revision_decision(&root_config, &workspace_root, &session_path)?;

    // Executing the revision runs a real plan review and commits the new draft.
    let mut recorded = Vec::new();
    let mut handler = RecordingRevisionHandler(std::mem::take(&mut recorded));
    let outcome = crate::application_run::execute_plan_review_revision(
        &root_config,
        &workspace_root,
        &session_path,
        &revision_request,
        &mut handler,
        None,
    )
    .await?;
    let PlanReviewRunOutcome::DraftReady {
        draft: revised_draft,
    } = outcome
    else {
        panic!("revision must commit a draft");
    };
    assert_eq!(revised_draft.summary, "Revised coordinator migration");
    assert!(!handler.0.is_empty(), "revision run must publish events");

    // The durable session now has the revision attempt in DraftReady with the new plan bound.
    let store = sigil_kernel::JsonlSessionStore::new(&session_path)?;
    let (_, fallback_route) =
        crate::provider_connections::resolve_default_model_route(&root_config)?;
    let reloaded =
        crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
            &root_config,
            &fallback_route,
            store,
            None,
            None,
            None,
        )?;
    let projection = sigil_kernel::PlanReviewProjection::from_entries(reloaded.entries());
    assert!(!projection.has_conflicts());
    let review_id = sigil_kernel::plan_review_id_for_source(
        &sigil_kernel::ConversationTurnRef::new(reloaded.session_scope_id(), "message-1", "run-1")?,
    );
    assert!(
        projection
            .latest_attempt(&review_id)
            .map(|attempt| attempt.status == sigil_kernel::PlanReviewAttemptStatus::DraftReady)
            .unwrap_or(false),
        "the revision attempt must terminate as DraftReady, not stay Started"
    );
    assert!(
        reloaded
            .plan_artifact_projection()
            .plans
            .contains_key(&revised_draft.plan_id)
    );
    fixture.abort();
    Ok(())
}

#[tokio::test]
async fn revision_provider_construction_failure_closes_the_attempt_terminal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let session_path = temp.path().join("session.jsonl");
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "local-test"
model = "gpt-4.1"
tool_timeout_secs = 5

[model_request]
request_timeout_secs = 0

[task]
routing_policy = "auto"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:9"
credential = { source = "none" }
"#,
    )?;
    let root_config: sigil_kernel::RootConfig =
        toml::from_str(&std::fs::read_to_string(&config_path)?)?;
    let workspace_root = sigil_kernel::resolve_workspace_root(&config_path, &workspace, ".");
    let revision_request = seed_revision_decision(&root_config, &workspace_root, &session_path)?;

    let mut recorded = Vec::new();
    let mut handler = RecordingRevisionHandler(std::mem::take(&mut recorded));
    let outcome = crate::application_run::execute_plan_review_revision(
        &root_config,
        &workspace_root,
        &session_path,
        &revision_request,
        &mut handler,
        None,
    )
    .await;
    assert!(
        outcome.is_err(),
        "invalid request timeout must fail provider construction"
    );

    // The post-Started finalizer closes the attempt terminal instead of leaving a dangling
    // Started record that recovery would later guess as Interrupted.
    let store = sigil_kernel::JsonlSessionStore::new(&session_path)?;
    let (_, fallback_route) =
        crate::provider_connections::resolve_default_model_route(&root_config)?;
    let reloaded =
        crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
            &root_config,
            &fallback_route,
            store,
            None,
            None,
            None,
        )?;
    let projection = sigil_kernel::PlanReviewProjection::from_entries(reloaded.entries());
    let attempt = projection
        .latest_attempt(&revision_request.plan_review_id)
        .expect("revision attempt");
    assert_eq!(
        attempt.status,
        PlanReviewAttemptStatus::Failed,
        "provider-construction failure must close the attempt as Failed"
    );
    assert_eq!(
        attempt.terminal_reason,
        Some(sigil_kernel::PlanReviewTerminalReason::RunFailed)
    );
    Ok(())
}

#[tokio::test]
async fn revision_draft_commit_conflict_closes_the_attempt_terminal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let session_path = temp.path().join("session.jsonl");

    let (fixture, base_url) = spawn_revision_draft_fixture().await?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "local-test"
model = "gpt-4.1"
tool_timeout_secs = 5

[task]
routing_policy = "auto"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "{base_url}"
credential = {{ source = "none" }}
"#
        ),
    )?;
    let root_config: sigil_kernel::RootConfig =
        toml::from_str(&std::fs::read_to_string(&config_path)?)?;
    let workspace_root = sigil_kernel::resolve_workspace_root(&config_path, &workspace, ".");
    let revision_request = seed_revision_decision(&root_config, &workspace_root, &session_path)?;

    // Pre-bind the revision plan id to conflicting durable facts so the draft commit fails
    // closed instead of silently replacing the plan.
    let store = sigil_kernel::JsonlSessionStore::new(&session_path)?;
    let (_, fallback_route) =
        crate::provider_connections::resolve_default_model_route(&root_config)?;
    let mut session =
        crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
            &root_config,
            &fallback_route,
            store,
            None,
            None,
            None,
        )?;
    let mut conflicting = draft_entry(&revision_request);
    conflicting.summary = "conflicting pre-existing draft".to_owned();
    session.append_control(sigil_kernel::ControlEntry::PlanDraftCreated(conflicting))?;
    drop(session);

    let mut recorded = Vec::new();
    let mut handler = RecordingRevisionHandler(std::mem::take(&mut recorded));
    let outcome = crate::application_run::execute_plan_review_revision(
        &root_config,
        &workspace_root,
        &session_path,
        &revision_request,
        &mut handler,
        None,
    )
    .await;
    assert!(
        outcome.is_err(),
        "conflicting durable draft facts must fail the commit"
    );

    // The post-Started finalizer closes the attempt terminal instead of leaving a dangling
    // Started record that recovery would later guess as Interrupted.
    let store = sigil_kernel::JsonlSessionStore::new(&session_path)?;
    let reloaded =
        crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
            &root_config,
            &fallback_route,
            store,
            None,
            None,
            None,
        )?;
    let projection = sigil_kernel::PlanReviewProjection::from_entries(reloaded.entries());
    let attempt = projection
        .latest_attempt(&revision_request.plan_review_id)
        .expect("revision attempt");
    assert_eq!(
        attempt.status,
        PlanReviewAttemptStatus::Failed,
        "commit conflict must close the attempt as Failed"
    );
    assert_eq!(
        attempt.terminal_reason,
        Some(sigil_kernel::PlanReviewTerminalReason::RunFailed)
    );
    fixture.abort();
    Ok(())
}

#[test]
fn prepare_automatic_plan_review_validates_and_starts_attempt() -> Result<()> {
    let (mut session, request) = session_with_route_decision()?;
    let action = sigil_kernel::StartPlanReviewAction {
        decision_id: request.route_decision_id.clone().expect("decision id"),
        plan_review_id: request.plan_review_id.clone(),
        plan_id: request.plan_id.clone(),
        source_turn: request.source_turn.clone(),
    };
    let prepared =
        PlanReviewCoordinator::prepare_automatic_plan_review(&mut session, &action, None, 100)?;
    assert_eq!(prepared.plan_review_id, request.plan_review_id);
    assert_eq!(prepared.attempt_id, request.attempt_id);
    assert_eq!(
        prepared.objective,
        "design the migration before touching anything"
    );
    assert_eq!(
        prepared.source,
        PlanReviewSource::AutomaticConversationRoute
    );

    let projection = PlanReviewProjection::from_entries(session.entries());
    let attempt = projection
        .latest_attempt(&request.plan_review_id)
        .expect("attempt");
    assert_eq!(attempt.status, PlanReviewAttemptStatus::Started);
    assert_eq!(attempt.source, PlanReviewSource::AutomaticConversationRoute);
    assert_eq!(
        attempt.route_decision_id.as_ref(),
        Some(&request.route_decision_id.clone().expect("decision id"))
    );

    // Idempotent prepare does not duplicate the attempt.
    let again =
        PlanReviewCoordinator::prepare_automatic_plan_review(&mut session, &action, None, 101)?;
    assert_eq!(again.attempt_id, request.attempt_id);
    let projection = PlanReviewProjection::from_entries(session.entries());
    assert_eq!(
        projection
            .review(&request.plan_review_id)
            .expect("review")
            .attempts
            .len(),
        1
    );
    Ok(())
}

#[test]
fn prepare_automatic_plan_review_fails_closed_on_missing_decision() -> Result<()> {
    let mut session = Session::new("plan-review-test", "planned-model");
    let mut message = ModelMessage::user("design first");
    message.id = "user-1".to_owned();
    session.append_user_message(message)?;
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        "user-1".to_owned(),
        "plan-review-run",
    )?;
    let plan_review_id = sigil_kernel::plan_review_id_for_source(&source_turn);
    let attempt_id = plan_review_attempt_id_for_review(&plan_review_id);
    let plan_id = plan_review_plan_id_for_attempt(&plan_review_id, &attempt_id);
    let action = sigil_kernel::StartPlanReviewAction {
        decision_id: sigil_kernel::ConversationRouteDecisionId::new("route-missing")?,
        plan_review_id,
        plan_id,
        source_turn,
    };
    assert!(
        PlanReviewCoordinator::prepare_automatic_plan_review(&mut session, &action, None, 100)
            .is_err()
    );
    assert!(session.entries().iter().all(|entry| {
        !matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(_))
        )
    }));
    Ok(())
}

#[test]
fn prepare_explicit_plan_review_uses_host_derived_identity() -> Result<()> {
    let mut session = Session::new("plan-review-test", "planned-model");
    let prepared = PlanReviewCoordinator::prepare_explicit_plan_review(
        &mut session,
        "draft the RFC",
        "run-1",
        None,
        100,
    )?;
    assert_eq!(prepared.source, PlanReviewSource::ExplicitPlanCommand);
    assert!(prepared.route_decision_id.is_none());
    assert_eq!(prepared.objective, "draft the RFC");
    let projection = PlanReviewProjection::from_entries(session.entries());
    let attempt = projection
        .latest_attempt(&prepared.plan_review_id)
        .expect("attempt");
    assert_eq!(attempt.status, PlanReviewAttemptStatus::Started);
    assert_eq!(attempt.source, PlanReviewSource::ExplicitPlanCommand);

    // Same session + logical run derives the same identity (retry-stable).
    let again = PlanReviewCoordinator::prepare_explicit_plan_review(
        &mut session,
        "draft the RFC",
        "run-1",
        None,
        101,
    )?;
    assert_eq!(again.plan_review_id, prepared.plan_review_id);
    assert_eq!(again.attempt_id, prepared.attempt_id);
    Ok(())
}

#[test]
fn commit_draft_is_idempotent_and_conflicts_fail_closed() -> Result<()> {
    let (mut session, request) = session_with_route_decision()?;
    let action = sigil_kernel::StartPlanReviewAction {
        decision_id: request.route_decision_id.clone().expect("decision id"),
        plan_review_id: request.plan_review_id.clone(),
        plan_id: request.plan_id.clone(),
        source_turn: request.source_turn.clone(),
    };
    PlanReviewCoordinator::prepare_automatic_plan_review(&mut session, &action, None, 100)?;
    let draft = draft_entry(&request);

    PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        110,
    )?;
    let projection = PlanReviewProjection::from_entries(session.entries());
    let attempt = projection
        .latest_attempt(&request.plan_review_id)
        .expect("attempt");
    assert_eq!(attempt.status, PlanReviewAttemptStatus::DraftReady);
    assert_eq!(
        session
            .plan_artifact_projection()
            .plans
            .get(&request.plan_id),
        Some(&draft)
    );

    // Identical re-commit is idempotent.
    PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        111,
    )?;
    let projection = PlanReviewProjection::from_entries(session.entries());
    assert_eq!(
        projection
            .review(&request.plan_review_id)
            .expect("review")
            .attempts
            .len(),
        2
    );

    // Conflicting draft facts fail closed.
    let mut conflicting = draft.clone();
    conflicting.summary = "Different summary".to_owned();
    assert!(
        PlanReviewCoordinator::commit_draft_from_child(
            &mut session,
            &conflicting,
            &request,
            &test_plan_compile_input(),
            112,
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn complete_without_draft_closes_automatic_review() -> Result<()> {
    let (mut session, request) = session_with_route_decision()?;
    let action = sigil_kernel::StartPlanReviewAction {
        decision_id: request.route_decision_id.clone().expect("decision id"),
        plan_review_id: request.plan_review_id.clone(),
        plan_id: request.plan_id.clone(),
        source_turn: request.source_turn.clone(),
    };
    PlanReviewCoordinator::prepare_automatic_plan_review(&mut session, &action, None, 100)?;
    PlanReviewCoordinator::complete_without_draft(&mut session, &request, 120)?;
    let projection = PlanReviewProjection::from_entries(session.entries());
    let attempt = projection
        .latest_attempt(&request.plan_review_id)
        .expect("attempt");
    assert_eq!(
        attempt.status,
        PlanReviewAttemptStatus::CompletedWithoutDraft
    );
    assert_eq!(
        attempt.terminal_reason,
        Some(sigil_kernel::PlanReviewTerminalReason::NoDraftAfterRetry)
    );
    // Terminal review rejects further attempts.
    assert!(PlanReviewCoordinator::complete_without_draft(&mut session, &request, 121).is_ok());
    let projection = PlanReviewProjection::from_entries(session.entries());
    assert!(projection.is_terminal(&request.plan_review_id));
    Ok(())
}

#[test]
fn record_plan_decision_is_typed_idempotent_and_stale_safe() -> Result<()> {
    let (mut session, request) = session_with_route_decision()?;
    let action = sigil_kernel::StartPlanReviewAction {
        decision_id: request.route_decision_id.clone().expect("decision id"),
        plan_review_id: request.plan_review_id.clone(),
        plan_id: request.plan_id.clone(),
        source_turn: request.source_turn.clone(),
    };
    PlanReviewCoordinator::prepare_automatic_plan_review(&mut session, &action, None, 100)?;
    let draft = draft_entry(&request);
    PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        110,
    )?;

    let saved = PlanDecisionCommand {
        plan_id: request.plan_id.as_str().to_owned(),
        expected_plan_hash: draft.plan_hash.clone(),
        decision: PlanDecision::SavedOnly,
    };
    let entry = PlanReviewCoordinator::record_plan_decision(&mut session, &saved, 200)?;
    assert_eq!(entry.decision, PlanDecision::SavedOnly);
    assert_eq!(entry.plan_hash, draft.plan_hash);
    // Idempotent replay.
    let again = PlanReviewCoordinator::record_plan_decision(&mut session, &saved, 201)?;
    assert_eq!(again.decision, PlanDecision::SavedOnly);

    // Stale hash fails closed.
    let stale = PlanDecisionCommand {
        plan_id: request.plan_id.as_str().to_owned(),
        expected_plan_hash: "sha256:old".to_owned(),
        decision: PlanDecision::Rejected,
    };
    assert!(PlanReviewCoordinator::record_plan_decision(&mut session, &stale, 202).is_err());

    // Conflicting decision fails closed.
    let rejected = PlanDecisionCommand {
        plan_id: request.plan_id.as_str().to_owned(),
        expected_plan_hash: draft.plan_hash.clone(),
        decision: PlanDecision::Rejected,
    };
    assert!(PlanReviewCoordinator::record_plan_decision(&mut session, &rejected, 203).is_err());
    Ok(())
}

#[test]
fn saved_plan_remains_revisable_and_rejectable_through_durable_transitions() -> Result<()> {
    let (mut revisable, _request, saved_draft) = session_with_ready_plan()?;
    PlanReviewCoordinator::record_plan_decision(
        &mut revisable,
        &PlanDecisionCommand {
            plan_id: saved_draft.plan_id.as_str().to_owned(),
            expected_plan_hash: saved_draft.plan_hash.clone(),
            decision: PlanDecision::SavedOnly,
        },
        20,
    )?;
    let revision = submit_revision_guidance(&mut revisable, &saved_draft)?;
    assert_eq!(
        revision.base_plan_id.as_ref(),
        Some(&saved_draft.plan_id),
        "a saved plan must remain a valid revision base"
    );
    assert_eq!(
        revisable
            .plan_artifact_projection()
            .latest_decision(&saved_draft.plan_id)
            .expect("revision decision")
            .decision,
        PlanDecision::RevisionRequested
    );

    let (mut rejectable, _request, rejected_draft) = session_with_ready_plan()?;
    PlanReviewCoordinator::record_plan_decision(
        &mut rejectable,
        &PlanDecisionCommand {
            plan_id: rejected_draft.plan_id.as_str().to_owned(),
            expected_plan_hash: rejected_draft.plan_hash.clone(),
            decision: PlanDecision::SavedOnly,
        },
        30,
    )?;
    let rejected = PlanReviewCoordinator::reject_plan(
        &mut rejectable,
        &crate::RejectPlanRequest {
            plan_id: rejected_draft.plan_id.as_str().to_owned(),
            expected_plan_hash: rejected_draft.plan_hash.clone(),
        },
    )?;
    assert_eq!(rejected.entry.decision, PlanDecision::Rejected);
    let replayed = PlanReviewCoordinator::reject_plan(
        &mut rejectable,
        &crate::RejectPlanRequest {
            plan_id: rejected_draft.plan_id.as_str().to_owned(),
            expected_plan_hash: rejected_draft.plan_hash,
        },
    )?;
    assert_eq!(replayed.entry, rejected.entry);
    Ok(())
}

#[test]
fn create_task_from_plan_promotes_valid_draft_and_is_idempotent() -> Result<()> {
    let (mut session, request) = session_with_route_decision()?;
    let action = sigil_kernel::StartPlanReviewAction {
        decision_id: request.route_decision_id.clone().expect("decision id"),
        plan_review_id: request.plan_review_id.clone(),
        plan_id: request.plan_id.clone(),
        source_turn: request.source_turn.clone(),
    };
    PlanReviewCoordinator::prepare_automatic_plan_review(&mut session, &action, None, 100)?;
    let draft = draft_entry(&request);
    PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        110,
    )?;

    let root_config: sigil_kernel::RootConfig = toml::from_str(
        r#"
config_version = 2

[agent]
connection = "deepseek"
model = "deepseek-v4-flash"
"#,
    )?;
    let temp = tempfile::tempdir()?;
    let parent_session_ref = SessionRef::new_relative("session.jsonl")?;
    let create = crate::plan_review_coordinator::CreateTaskFromPlanRequest {
        plan_id: request.plan_id.as_str().to_owned(),
        expected_plan_hash: draft.plan_hash.clone(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreatePaused,
        permission_grant: None,
    };
    let created = PlanReviewCoordinator::create_task_from_plan(
        &mut session,
        &root_config,
        temp.path(),
        parent_session_ref.clone(),
        &create,
    )?;
    assert_eq!(created.entry.plan_id, request.plan_id);
    assert_eq!(
        created.entry.task_plan_version, 0,
        "incomplete draft uses compatibility planner"
    );
    let task = session
        .task_state_projection()
        .tasks
        .get(&created.task_id)
        .cloned()
        .expect("task exists");
    assert_eq!(task.status, TaskRunStatus::Paused);
    assert_eq!(
        session
            .plan_artifact_projection()
            .latest_decision(&request.plan_id)
            .map(|entry| entry.decision),
        Some(PlanDecision::Accepted)
    );

    // Idempotent retry reconciles the deterministic prefix.
    let again = PlanReviewCoordinator::create_task_from_plan(
        &mut session,
        &root_config,
        temp.path(),
        parent_session_ref,
        &create,
    )?;
    assert_eq!(again.task_id, created.task_id);

    // A draft bound to the exact current workspace snapshot is direct-promoted.
    let (mut bound_session, unbound_request) = session_with_route_decision()?;
    let bound_action = sigil_kernel::StartPlanReviewAction {
        decision_id: unbound_request
            .route_decision_id
            .clone()
            .expect("decision id"),
        plan_review_id: unbound_request.plan_review_id.clone(),
        plan_id: unbound_request.plan_id.clone(),
        source_turn: unbound_request.source_turn.clone(),
    };
    let snapshot = crate::plan_handoff_workspace_snapshot_id(&root_config, temp.path())?
        .expect("workspace snapshot");
    let bound_request = PlanReviewCoordinator::prepare_automatic_plan_review(
        &mut bound_session,
        &bound_action,
        Some(snapshot),
        100,
    )?;
    let mut bound_draft = draft_entry(&bound_request);
    bound_draft.steps = vec![sigil_kernel::PlanDraftStep {
        step_id: "migrate_1".to_owned(),
        title: "Migrate coordinator".to_owned(),
        display_name: None,
        detail: None,
        role: Some(sigil_kernel::AgentRole::Executor),
        depends_on: Vec::new(),
        intent_aliases: Vec::new(),
        mode: Some(sigil_kernel::TaskStepMode::Write),
        isolation: Some(sigil_kernel::TaskIsolationMode::SequentialWorkspaceWrite),
        target_paths: vec!["src/coordinator.rs".to_owned()],
        required_capabilities: Vec::new(),
        deliverables: Vec::new(),
        acceptance_criteria: Vec::new(),
        suggested_checks: Vec::new(),
        risk: None,
        notes: Vec::new(),
    }];
    PlanReviewCoordinator::commit_draft_from_child(
        &mut bound_session,
        &bound_draft,
        &bound_request,
        &test_plan_compile_input(),
        110,
    )?;
    let bound_create = crate::plan_review_coordinator::CreateTaskFromPlanRequest {
        plan_id: bound_request.plan_id.as_str().to_owned(),
        expected_plan_hash: bound_draft.plan_hash.clone(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreatePaused,
        permission_grant: None,
    };
    let promoted = PlanReviewCoordinator::create_task_from_plan(
        &mut bound_session,
        &root_config,
        temp.path(),
        SessionRef::new_relative("session.jsonl")?,
        &bound_create,
    )?;
    assert_eq!(
        promoted.entry.task_plan_version, 1,
        "a draft bound to the unchanged workspace snapshot must direct-promote without the compatibility planner"
    );
    assert_eq!(
        promoted.entry.stale_reason, None,
        "the bound draft must not be reported stale"
    );

    // A changed workspace snapshot on a fresh session degrades to the compatibility planner
    // with a durable stale reason (the same-session retry after a direct promotion fails closed).
    let (mut drift_session, unbound_drift_request) = session_with_route_decision()?;
    let drift_action = sigil_kernel::StartPlanReviewAction {
        decision_id: unbound_drift_request
            .route_decision_id
            .clone()
            .expect("decision id"),
        plan_review_id: unbound_drift_request.plan_review_id.clone(),
        plan_id: unbound_drift_request.plan_id.clone(),
        source_turn: unbound_drift_request.source_turn.clone(),
    };
    let drift_snapshot = crate::plan_handoff_workspace_snapshot_id(&root_config, temp.path())?
        .expect("workspace snapshot");
    let drift_request = PlanReviewCoordinator::prepare_automatic_plan_review(
        &mut drift_session,
        &drift_action,
        Some(drift_snapshot),
        100,
    )?;
    let mut drift_draft = draft_entry(&drift_request);
    drift_draft.steps = bound_draft.steps.clone();
    PlanReviewCoordinator::commit_draft_from_child(
        &mut drift_session,
        &drift_draft,
        &drift_request,
        &test_plan_compile_input(),
        110,
    )?;
    let changed_root = tempfile::tempdir()?;
    std::fs::write(changed_root.path().join("marker.txt"), b"changed")?;
    let drift_create = crate::plan_review_coordinator::CreateTaskFromPlanRequest {
        plan_id: drift_request.plan_id.as_str().to_owned(),
        expected_plan_hash: drift_draft.plan_hash.clone(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreatePaused,
        permission_grant: None,
    };
    let stale_promoted = PlanReviewCoordinator::create_task_from_plan(
        &mut drift_session,
        &root_config,
        changed_root.path(),
        SessionRef::new_relative("session.jsonl")?,
        &drift_create,
    )?;
    assert_eq!(
        stale_promoted.entry.task_plan_version, 0,
        "a changed workspace must fall back to the compatibility planner"
    );
    assert!(
        stale_promoted.entry.stale_reason.is_some(),
        "a changed workspace must surface the stale reason durably"
    );

    // Stale hash fails closed.
    let stale = crate::plan_review_coordinator::CreateTaskFromPlanRequest {
        plan_id: request.plan_id.as_str().to_owned(),
        expected_plan_hash: "sha256:old".to_owned(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreatePaused,
        permission_grant: None,
    };
    assert!(
        PlanReviewCoordinator::create_task_from_plan(
            &mut session,
            &root_config,
            temp.path(),
            SessionRef::new_relative("session.jsonl")?,
            &stale,
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn failed_task_creation_is_durable_and_the_same_plan_can_retry() -> Result<()> {
    let (mut session, request) = session_with_route_decision()?;
    let action = sigil_kernel::StartPlanReviewAction {
        decision_id: request.route_decision_id.clone().expect("decision id"),
        plan_review_id: request.plan_review_id.clone(),
        plan_id: request.plan_id.clone(),
        source_turn: request.source_turn.clone(),
    };
    PlanReviewCoordinator::prepare_automatic_plan_review(&mut session, &action, None, 100)?;
    let mut draft = draft_entry(&request);
    draft.target_paths.clear();
    PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        110,
    )?;

    let root_config: sigil_kernel::RootConfig = toml::from_str(
        r#"
config_version = 2

[agent]
connection = "deepseek"
model = "deepseek-v4-flash"
"#,
    )?;
    let temp = tempfile::tempdir()?;
    let mut create = crate::plan_review_coordinator::CreateTaskFromPlanRequest {
        plan_id: request.plan_id.as_str().to_owned(),
        expected_plan_hash: draft.plan_hash.clone(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreatePaused,
        permission_grant: Some(sigil_kernel::PlanApprovalPermission::WorkspaceEdits),
    };
    let error = PlanReviewCoordinator::create_task_from_plan(
        &mut session,
        &root_config,
        temp.path(),
        SessionRef::new_relative("session.jsonl")?,
        &create,
    )
    .expect_err("a scoped edit grant needs concrete target paths");
    assert!(error.to_string().contains("no concrete target paths"));

    let projection = session.plan_artifact_projection();
    let failure = projection
        .latest_decision(&request.plan_id)
        .expect("failure settlement");
    assert_eq!(failure.decision, PlanDecision::TaskCreationFailed);
    assert!(
        failure
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("no concrete target paths"))
    );
    assert_eq!(projection.latest_pending_plan(), Some(&draft));

    create.permission_grant = None;
    let retried = PlanReviewCoordinator::create_task_from_plan(
        &mut session,
        &root_config,
        temp.path(),
        SessionRef::new_relative("session.jsonl")?,
        &create,
    )?;
    assert_eq!(retried.entry.plan_id, request.plan_id);
    assert_eq!(
        session
            .plan_artifact_projection()
            .latest_decision(&request.plan_id)
            .map(|decision| decision.decision),
        Some(PlanDecision::Accepted)
    );
    Ok(())
}

#[test]
fn reject_plan_is_durable_and_prevents_task_creation() -> Result<()> {
    let (mut session, request) = session_with_route_decision()?;
    let action = sigil_kernel::StartPlanReviewAction {
        decision_id: request.route_decision_id.clone().expect("decision id"),
        plan_review_id: request.plan_review_id.clone(),
        plan_id: request.plan_id.clone(),
        source_turn: request.source_turn.clone(),
    };
    PlanReviewCoordinator::prepare_automatic_plan_review(&mut session, &action, None, 100)?;
    let draft = draft_entry(&request);
    PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        110,
    )?;

    let rejected = PlanReviewCoordinator::reject_plan(
        &mut session,
        &crate::RejectPlanRequest {
            plan_id: request.plan_id.as_str().to_owned(),
            expected_plan_hash: draft.plan_hash.clone(),
        },
    )?;
    assert_eq!(rejected.entry.decision, PlanDecision::Rejected);
    assert!(
        session
            .plan_artifact_projection()
            .plan_is_rejected(&request.plan_id)
    );

    let create = crate::plan_review_coordinator::CreateTaskFromPlanRequest {
        plan_id: request.plan_id.as_str().to_owned(),
        expected_plan_hash: draft.plan_hash.clone(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreatePaused,
        permission_grant: None,
    };
    let root_config: sigil_kernel::RootConfig = toml::from_str(
        r#"
config_version = 2

[agent]
connection = "deepseek"
model = "deepseek-v4-flash"
"#,
    )?;
    let temp = tempfile::tempdir()?;
    assert!(
        PlanReviewCoordinator::create_task_from_plan(
            &mut session,
            &root_config,
            temp.path(),
            SessionRef::new_relative("session.jsonl")?,
            &create,
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn plan_review_binding_identities_are_stable_across_rebinding() -> Result<()> {
    let mut session = Session::new("plan-review-test", "planned-model");
    let mut message = ModelMessage::user("design first");
    message.id = "user-1".to_owned();
    session.append_user_message(message)?;
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        "user-1".to_owned(),
        "plan-review-run",
    )?;
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let bound = coordinator.bind_conversation_input(
        &session,
        sigil_kernel::AgentRunInput::user("design first"),
        SessionRef::new_relative("session.jsonl")?,
        "plan-review-run",
        Some(crate::ConversationSourceTurn {
            message_id: "user-1".to_owned(),
            objective: "design first".to_owned(),
        }),
        42,
    )?;
    let AgentRunPurpose::Conversation(context) = bound.purpose.expect("conversation purpose")
    else {
        panic!("expected conversation purpose");
    };
    let review = context.plan_review.expect("plan review binding");
    assert_eq!(
        review.plan_review_id,
        sigil_kernel::plan_review_id_for_source(&source_turn)
    );
    assert_eq!(
        review.decision_id,
        sigil_kernel::conversation_route_decision_id_for_source(&source_turn)
    );
    assert_eq!(
        review.plan_id,
        plan_review_plan_id_for_attempt(&review.plan_review_id, &review.attempt_id)
    );
    assert_eq!(
        review.policy_snapshot_hash,
        plan_review_policy_snapshot_hash()
    );
    assert_eq!(review.requested_at_ms, 42);
    Ok(())
}

// --- RFC-0067 single execution spine integration tests ---

fn write_admission_test_config(root: &std::path::Path) -> std::path::PathBuf {
    let path = root.join("sigil.toml");
    std::fs::create_dir_all(root).expect("test root");
    std::fs::write(
        &path,
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

[task]
enabled = true
max_plan_steps = 64
"#,
    )
    .expect("test config");
    path
}

fn spine_session(
    plan_id: &str,
    summary: &str,
) -> Result<(Session, PlanDraftCreatedEntry, tempfile::TempDir)> {
    let temp = tempfile::tempdir()?;
    let session = Session::new("mock", "model");
    let store = JsonlSessionStore::new(temp.path().join("spine.jsonl"))?;
    let mut session = session.with_store(store);
    let draft = sigil_kernel::PlanDraftCreatedEntry {
        plan_id: sigil_kernel::PlanId::new(plan_id.to_owned())?,
        schema_version: 2,
        source: sigil_kernel::PlanSourceRef::default(),
        plan_hash: sigil_kernel::plan_text_hash(summary),
        summary: summary.to_owned(),
        inline_text: None,
        steps: vec![sigil_kernel::PlanDraftStep {
            step_id: "step_1".to_owned(),
            title: "Implement the change".to_owned(),
            display_name: None,
            detail: None,
            role: Some(sigil_kernel::AgentRole::Executor),
            depends_on: Vec::new(),
            intent_aliases: Vec::new(),
            mode: Some(sigil_kernel::TaskStepMode::Write),
            isolation: Some(sigil_kernel::TaskIsolationMode::SequentialWorkspaceWrite),
            target_paths: vec!["src/lib.rs".to_owned()],
            required_capabilities: Vec::new(),
            deliverables: Vec::new(),
            acceptance_criteria: Vec::new(),
            suggested_checks: Vec::new(),
            risk: None,
            notes: Vec::new(),
        }],
        intent_proposal: None,
        target_paths: vec!["src/lib.rs".to_owned()],
        suggested_checks: Vec::new(),
        risk: None,
        notes: Vec::new(),
        workspace_snapshot_id: None,
        created_at_ms: 10,
    };
    session.append_control(ControlEntry::PlanDraftCreated(draft.clone()))?;
    Ok((session, draft, temp))
}

fn make_ready(session: &mut Session, draft: &PlanDraftCreatedEntry) -> Result<String> {
    let candidate =
        sigil_kernel::compile_executable_plan_candidate(draft, &test_plan_compile_input())
            .expect("fixture must compile");
    let marker = sigil_kernel::PlanReadyCommittedV1Entry {
        plan_id: draft.plan_id.clone(),
        plan_hash: draft.plan_hash.clone(),
        candidate_hash: candidate.candidate_hash.clone(),
        attempt_id: "attempt-1".to_owned(),
        committed_at_ms: 20,
    };
    session.append_control(ControlEntry::ExecutablePlanCandidatePreparedV1(Box::new(
        candidate.clone(),
    )))?;
    session.append_control(ControlEntry::PlanReadyCommittedV1(marker))?;
    Ok(candidate.candidate_hash)
}

#[test]
fn plan_execution_service_adopts_atomically_and_idempotently() -> Result<()> {
    let (mut session, draft, _temp) = spine_session("plan_spine_1", "Adopt atomically")?;
    let candidate_hash = make_ready(&mut session, &draft)?;
    let parent_ref = SessionRef::new_relative("parent.jsonl")?;
    let command = sigil_kernel::PlanRunCommandV1 {
        command_id: "run-command-1".to_owned(),
        session_id: session.session_scope_id().to_owned(),
        plan_id: draft.plan_id.clone(),
        expected_plan_hash: draft.plan_hash.clone(),
        expected_candidate_hash: candidate_hash.clone(),
        expected_durable_frontier: session.durable_frontier_sequence(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreateAndRun,
        permission: sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy,
        source: sigil_kernel::PlanRunCommandSource::TuiKeyboard,
    };
    let receipt =
        match crate::PlanExecutionService::adopt(&mut session, parent_ref.clone(), &command, 30) {
            Ok(receipt) => receipt,
            Err(rejection) => anyhow::bail!(
                "adoption rejected: {}",
                crate::plan_run_rejection_message(&rejection)
            ),
        };
    assert!(!receipt.already_adopted);
    assert_eq!(receipt.plan_id, draft.plan_id);
    assert_eq!(receipt.candidate_hash, candidate_hash);
    assert_eq!(
        receipt.initial_phase,
        sigil_kernel::TaskExecutionPhaseV1::Preparing
    );

    let artifacts = session.plan_artifact_projection();
    assert_eq!(artifacts.adoptions.len(), 1);
    assert_eq!(
        artifacts
            .adoptions
            .get(&draft.plan_id)
            .expect("adoption must be recorded")
            .len(),
        1
    );
    // The task is visible immediately from the adoption event.
    let tasks = session.task_state_projection();
    let task = tasks.tasks.get(&receipt.task_id).expect("adopted task");
    assert_eq!(task.status, sigil_kernel::TaskRunStatus::Started);
    assert_eq!(
        tasks.execution_phase(&receipt.task_id),
        Some(sigil_kernel::TaskExecutionPhaseV1::Preparing)
    );

    // Same command retry returns the same receipt idempotently.
    let retried =
        match crate::PlanExecutionService::adopt(&mut session, parent_ref.clone(), &command, 31) {
            Ok(receipt) => receipt,
            Err(rejection) => anyhow::bail!(
                "adoption rejected: {}",
                crate::plan_run_rejection_message(&rejection)
            ),
        };
    assert!(retried.already_adopted);
    assert_eq!(retried.task_id, receipt.task_id);
    assert_eq!(retried.receipt_id, receipt.receipt_id);
    // Only one adoption event exists.
    assert_eq!(session.plan_artifact_projection().adoptions.len(), 1);

    // A different command for the same candidate returns the same task with already_adopted.
    let mut other = command.clone();
    other.command_id = "run-command-2".to_owned();
    let adopted_again =
        match crate::PlanExecutionService::adopt(&mut session, parent_ref, &other, 32) {
            Ok(receipt) => receipt,
            Err(rejection) => anyhow::bail!(
                "adoption rejected: {}",
                crate::plan_run_rejection_message(&rejection)
            ),
        };
    assert!(adopted_again.already_adopted);
    assert_eq!(adopted_again.task_id, receipt.task_id);
    Ok(())
}

#[test]
fn plan_execution_service_rejects_typed_without_consuming_the_plan() -> Result<()> {
    let (mut session, draft, _temp) = spine_session("plan_spine_2", "Reject typed")?;
    let candidate_hash = make_ready(&mut session, &draft)?;
    let parent_ref = SessionRef::new_relative("parent.jsonl")?;
    let base = sigil_kernel::PlanRunCommandV1 {
        command_id: "run-command-reject".to_owned(),
        session_id: session.session_scope_id().to_owned(),
        plan_id: draft.plan_id.clone(),
        expected_plan_hash: draft.plan_hash.clone(),
        expected_candidate_hash: candidate_hash.clone(),
        expected_durable_frontier: session.durable_frontier_sequence(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreateAndRun,
        permission: sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy,
        source: sigil_kernel::PlanRunCommandSource::Http,
    };
    // Stale candidate hash is rejected; the plan stays actionable.
    let mut stale_hash = base.clone();
    stale_hash.expected_candidate_hash = "sha256:stale".to_owned();
    let rejection =
        match crate::PlanExecutionService::adopt(&mut session, parent_ref.clone(), &stale_hash, 30)
        {
            Ok(_) => panic!("stale candidate must be rejected"),
            Err(rejection) => rejection,
        };
    assert_eq!(rejection.reason_code(), "candidate_hash_mismatch");
    // Stale frontier is rejected.
    let mut stale_frontier = base.clone();
    stale_frontier.expected_durable_frontier = 0;
    let rejection = match crate::PlanExecutionService::adopt(
        &mut session,
        parent_ref.clone(),
        &stale_frontier,
        30,
    ) {
        Ok(_) => panic!("stale frontier must be rejected"),
        Err(rejection) => rejection,
    };
    assert_eq!(rejection.reason_code(), "frontier_stale");
    // Not-ready plan (no marker) is rejected with a typed state.
    let (mut not_ready, other_draft, _temp) = spine_session("plan_spine_3", "Not ready")?;
    let parent_ref = SessionRef::new_relative("parent.jsonl")?;
    let not_ready_command = sigil_kernel::PlanRunCommandV1 {
        command_id: "run-command-not-ready".to_owned(),
        session_id: not_ready.session_scope_id().to_owned(),
        plan_id: other_draft.plan_id.clone(),
        expected_plan_hash: other_draft.plan_hash.clone(),
        expected_candidate_hash: "sha256:any".to_owned(),
        expected_durable_frontier: not_ready.durable_frontier_sequence(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreateAndRun,
        permission: sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy,
        source: sigil_kernel::PlanRunCommandSource::Http,
    };
    let rejection = match crate::PlanExecutionService::adopt(
        &mut not_ready,
        parent_ref,
        &not_ready_command,
        30,
    ) {
        Ok(_) => panic!("not-ready plan must be rejected"),
        Err(rejection) => rejection,
    };
    assert_eq!(rejection.reason_code(), "plan_not_ready");
    // The plan is not consumed by rejections.
    assert!(session.plan_artifact_projection().adoptions.is_empty());
    Ok(())
}

#[test]
fn commit_draft_from_child_batches_candidate_and_ready_marker() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session = Session::new("mock", "model");
    let mut session = session.with_store(JsonlSessionStore::new(temp.path().join("spine.jsonl"))?);
    let request = PlanReviewRunRequest {
        plan_review_id: sigil_kernel::PlanReviewId::new("review-1")?,
        attempt_id: sigil_kernel::PlanReviewAttemptId::new("attempt-1")?,
        plan_id: sigil_kernel::PlanId::new("plan_spine_4")?,
        source: PlanReviewSource::ExplicitPlanCommand,
        source_turn: ConversationTurnRef {
            session_scope_id: "session-1".to_owned(),
            message_id: "message-1".to_owned(),
            logical_run_id: "run-1".to_owned(),
        },
        route_decision_id: None,
        child_session_ref: SessionRef::new_relative("child.jsonl")?,
        finalizer_session_ref: SessionRef::new_relative("finalizer.jsonl")?,
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        objective: "implement".to_owned(),
        workspace_snapshot_id: None,
    };
    let draft = draft_entry(&request);
    crate::PlanReviewCoordinator::ensure_attempt_started(&mut session, &request, 25)?;
    crate::PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        30,
    )?;
    let artifacts = session.plan_artifact_projection();
    let candidate = artifacts
        .latest_candidate(&request.plan_id)
        .expect("candidate");
    let marker = artifacts
        .ready_markers
        .get(&request.plan_id)
        .expect("marker");
    assert_eq!(marker.candidate_hash, candidate.candidate_hash);
    assert_eq!(
        artifacts.plan_ready_state(&request.plan_id),
        sigil_kernel::PlanReadyStateV1::Ready
    );
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
            if attempt.status == PlanReviewAttemptStatus::DraftReady
    )));
    // Retry is idempotent and does not duplicate candidate/marker records.
    crate::PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        31,
    )?;
    let artifacts = session.plan_artifact_projection();
    assert_eq!(artifacts.candidates.len(), 1);
    assert_eq!(artifacts.ready_markers.len(), 1);
    Ok(())
}

#[test]
fn compile_failure_never_produces_draft_ready() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session = Session::new("mock", "model");
    let mut session = session.with_store(JsonlSessionStore::new(temp.path().join("spine.jsonl"))?);
    let request = PlanReviewRunRequest {
        plan_review_id: sigil_kernel::PlanReviewId::new("review-2")?,
        attempt_id: sigil_kernel::PlanReviewAttemptId::new("attempt-2")?,
        plan_id: sigil_kernel::PlanId::new("plan_spine_5")?,
        source: PlanReviewSource::ExplicitPlanCommand,
        source_turn: ConversationTurnRef {
            session_scope_id: "session-1".to_owned(),
            message_id: "message-1".to_owned(),
            logical_run_id: "run-1".to_owned(),
        },
        route_decision_id: None,
        child_session_ref: SessionRef::new_relative("child.jsonl")?,
        finalizer_session_ref: SessionRef::new_relative("finalizer.jsonl")?,
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        objective: "implement".to_owned(),
        workspace_snapshot_id: None,
    };
    let mut draft = draft_entry(&request);
    draft.steps[0].isolation = None;
    crate::PlanReviewCoordinator::ensure_attempt_started(&mut session, &request, 25)?;
    crate::PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        30,
    )?;
    let artifacts = session.plan_artifact_projection();
    assert_eq!(
        artifacts.plan_ready_state(&request.plan_id),
        sigil_kernel::PlanReadyStateV1::CompileFailed
    );
    assert!(!artifacts.ready_markers.contains_key(&request.plan_id));
    let failure = artifacts
        .compile_failures
        .get(&request.plan_id)
        .expect("compile failure must be recorded")
        .last()
        .expect("compile failure list must not be empty");
    assert_eq!(failure.reason_code, "incomplete_step_contract");
    let has_compile_failed_attempt = session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
                if attempt.status == PlanReviewAttemptStatus::CompileFailed
        )
    });
    assert!(has_compile_failed_attempt);
    // The plan detail is still readable and exposes the typed failure.
    let detail = sigil_kernel::plan_review_detail_from_entries(
        session.entries(),
        &request.plan_id,
        &draft.plan_hash,
    )?;
    assert_eq!(
        detail.compile.state,
        sigil_kernel::PlanReadyStateV1::CompileFailed
    );
    assert_eq!(
        detail
            .compile
            .failure
            .as_ref()
            .expect("compile failure detail")
            .reason_code,
        "incomplete_step_contract"
    );
    Ok(())
}

#[test]
fn admit_adopted_task_produces_typed_blockers_and_recovers_on_retry() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root_config = sigil_kernel::RootConfig::load(&write_admission_test_config(temp.path()))?;
    let base_snapshot = crate::plan_handoff_workspace_snapshot_id(&root_config, temp.path())?
        .expect("base snapshot");
    let (mut session, draft, _temp) = spine_session("plan_spine_6", "Admission flows")?;
    let store_draft = sigil_kernel::PlanDraftCreatedEntry {
        workspace_snapshot_id: Some(base_snapshot.clone()),
        ..draft.clone()
    };
    let candidate_hash = make_ready(&mut session, &store_draft)?;
    let parent_ref = SessionRef::new_relative("parent.jsonl")?;
    let command = sigil_kernel::PlanRunCommandV1 {
        command_id: "run-command-admission".to_owned(),
        session_id: session.session_scope_id().to_owned(),
        plan_id: draft.plan_id.clone(),
        expected_plan_hash: draft.plan_hash.clone(),
        expected_candidate_hash: candidate_hash.clone(),
        expected_durable_frontier: session.durable_frontier_sequence(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreateAndRun,
        permission: sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy,
        source: sigil_kernel::PlanRunCommandSource::TuiKeyboard,
    };
    let receipt = match crate::PlanExecutionService::adopt(&mut session, parent_ref, &command, 30) {
        Ok(receipt) => receipt,
        Err(rejection) => anyhow::bail!(
            "adoption rejected: {}",
            crate::plan_run_rejection_message(&rejection)
        ),
    };
    let candidate = session
        .plan_artifact_projection()
        .latest_candidate(&draft.plan_id)
        .cloned()
        .expect("candidate must remain durable");
    // Workspace drift blocks the task.
    std::fs::write(temp.path().join("marker.txt"), b"drift")?;
    let probes = crate::TaskAdmissionProbeContext {
        tool_contracts: Some(Vec::new()),
        provider_route_available: true,
        credential_available: true,
        permission_profile_ok: true,
        disk_space_bytes: None,
        verification_runner_available: true,
        external_writer_active: false,
    };
    let outcome = crate::admit_adopted_task(
        &mut session,
        &root_config,
        temp.path(),
        &receipt.task_id,
        &candidate,
        &probes,
        40,
    )?;
    let sigil_kernel::TaskAdmissionOutcomeV1::Blocked(blocker) = &outcome else {
        panic!("workspace drift must block the task");
    };
    assert_eq!(
        blocker.reason_code,
        sigil_kernel::TaskBlockerReasonCodeV1::WorkspaceChanged
    );
    assert!(blocker.retryable);
    let tasks = session.task_state_projection();
    assert_eq!(
        tasks.execution_phase(&receipt.task_id),
        Some(sigil_kernel::TaskExecutionPhaseV1::Blocked)
    );
    assert_eq!(tasks.next_admission_ordinal(&receipt.task_id), 2);
    // Missing required capability blocks with the exact capability.
    std::fs::remove_file(temp.path().join("marker.txt"))?;
    let probes = crate::TaskAdmissionProbeContext {
        tool_contracts: Some(Vec::new()),
        provider_route_available: true,
        credential_available: true,
        permission_profile_ok: true,
        disk_space_bytes: None,
        verification_runner_available: true,
        external_writer_active: false,
    };
    let outcome = crate::admit_adopted_task(
        &mut session,
        &root_config,
        temp.path(),
        &receipt.task_id,
        &candidate,
        &probes,
        41,
    )?;
    let sigil_kernel::TaskAdmissionOutcomeV1::Blocked(blocker) = &outcome else {
        panic!("missing capabilities must block the task");
    };
    assert_eq!(
        blocker.reason_code,
        sigil_kernel::TaskBlockerReasonCodeV1::MissingRequiredCapability
    );
    // Environment fixed + registry with the required capability → Ready on the next ordinal.
    let mut registry = ToolRegistry::new();
    sigil_tools_builtin::register_builtin_tools(&mut registry);
    let outcome = crate::admit_adopted_task(
        &mut session,
        &root_config,
        temp.path(),
        &receipt.task_id,
        &candidate,
        &crate::TaskAdmissionProbeContext {
            tool_contracts: Some(registry.contracts()),
            provider_route_available: true,
            credential_available: true,
            permission_profile_ok: true,
            disk_space_bytes: None,
            verification_runner_available: true,
            external_writer_active: false,
        },
        42,
    )?;
    assert!(matches!(
        outcome,
        sigil_kernel::TaskAdmissionOutcomeV1::Ready(_)
    ));
    let tasks = session.task_state_projection();
    assert_eq!(
        tasks.execution_phase(&receipt.task_id),
        Some(sigil_kernel::TaskExecutionPhaseV1::Ready)
    );
    assert_eq!(
        tasks
            .admission_attempts
            .get(&receipt.task_id)
            .expect("admission attempt must be recorded")
            .len(),
        3
    );
    Ok(())
}

#[test]
fn create_paused_adoption_stays_paused_without_probing() -> Result<()> {
    let (mut session, draft, _temp) = spine_session("plan_spine_7", "Create paused")?;
    let candidate_hash = make_ready(&mut session, &draft)?;
    let parent_ref = SessionRef::new_relative("parent.jsonl")?;
    let command = sigil_kernel::PlanRunCommandV1 {
        command_id: "run-command-paused".to_owned(),
        session_id: session.session_scope_id().to_owned(),
        plan_id: draft.plan_id.clone(),
        expected_plan_hash: draft.plan_hash.clone(),
        expected_candidate_hash: candidate_hash.clone(),
        expected_durable_frontier: session.durable_frontier_sequence(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreatePaused,
        permission: sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy,
        source: sigil_kernel::PlanRunCommandSource::TuiMouse,
    };
    let receipt = match crate::PlanExecutionService::adopt(&mut session, parent_ref, &command, 30) {
        Ok(receipt) => receipt,
        Err(rejection) => anyhow::bail!(
            "adoption rejected: {}",
            crate::plan_run_rejection_message(&rejection)
        ),
    };
    let candidate = session
        .plan_artifact_projection()
        .latest_candidate(&draft.plan_id)
        .cloned()
        .expect("candidate must remain durable");
    let temp = tempfile::tempdir()?;
    let root_config = sigil_kernel::RootConfig::load(&write_admission_test_config(temp.path()))?;
    let outcome = crate::admit_adopted_task(
        &mut session,
        &root_config,
        temp.path(),
        &receipt.task_id,
        &candidate,
        &crate::TaskAdmissionProbeContext::default(),
        40,
    )?;
    assert_eq!(
        outcome,
        sigil_kernel::TaskAdmissionOutcomeV1::Paused(sigil_kernel::TaskPauseReasonV1::CreatePaused)
    );
    let tasks = session.task_state_projection();
    assert_eq!(
        tasks.execution_phase(&receipt.task_id),
        Some(sigil_kernel::TaskExecutionPhaseV1::Paused)
    );
    assert_eq!(
        tasks
            .tasks
            .get(&receipt.task_id)
            .expect("paused task must project")
            .status,
        TaskRunStatus::Paused
    );
    Ok(())
}

// --- RFC-0067 audit closure tests ---

#[test]
fn admission_probes_observe_the_real_environment() -> Result<()> {
    let temp = tempfile::tempdir()?;
    // Config without any connection: the honest route probe must report the route unavailable.
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "local-test"
model = "gpt-test"

[task]
enabled = true
max_plan_steps = 64
"#,
    )?;
    let root_config = sigil_kernel::RootConfig::load(&config_path)?;
    let (mut session, draft, _temp) = spine_session("plan_spine_probe", "Probe environment")?;
    // Bind a real base snapshot so the workspace probe passes and the route probe decides.
    let base_snapshot = crate::plan_handoff_workspace_snapshot_id(&root_config, temp.path())?;
    let store_draft = sigil_kernel::PlanDraftCreatedEntry {
        workspace_snapshot_id: base_snapshot,
        ..draft
    };
    let candidate_hash = make_ready(&mut session, &store_draft)?;
    let parent_ref = SessionRef::new_relative("parent.jsonl")?;
    let command = sigil_kernel::PlanRunCommandV1 {
        command_id: "run-command-probe".to_owned(),
        session_id: session.session_scope_id().to_owned(),
        plan_id: store_draft.plan_id.clone(),
        expected_plan_hash: store_draft.plan_hash.clone(),
        expected_candidate_hash: candidate_hash.clone(),
        expected_durable_frontier: session.durable_frontier_sequence(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreateAndRun,
        permission: sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy,
        source: sigil_kernel::PlanRunCommandSource::TuiKeyboard,
    };
    let receipt = match crate::PlanExecutionService::adopt(&mut session, parent_ref, &command, 30) {
        Ok(receipt) => receipt,
        Err(rejection) => anyhow::bail!(
            "adoption rejected: {}",
            crate::plan_run_rejection_message(&rejection)
        ),
    };
    let candidate = session
        .plan_artifact_projection()
        .latest_candidate(&store_draft.plan_id)
        .cloned()
        .expect("candidate must remain durable");
    // The test config has no connections: the honest route probe must block.
    let probes = crate::build_task_admission_probes(
        &root_config,
        temp.path(),
        None,
        &session,
        &receipt.task_id,
        &candidate,
    );
    assert!(!probes.provider_route_available);
    assert!(!probes.credential_available);
    assert!(probes.permission_profile_ok);
    assert!(probes.verification_runner_available);
    let outcome = crate::admit_adopted_task(
        &mut session,
        &root_config,
        temp.path(),
        &receipt.task_id,
        &candidate,
        &probes,
        40,
    )?;
    let sigil_kernel::TaskAdmissionOutcomeV1::Blocked(blocker) = &outcome else {
        panic!("unresolvable route must block the task");
    };
    assert_eq!(
        blocker.reason_code,
        sigil_kernel::TaskBlockerReasonCodeV1::ProviderUnavailable
    );

    // A config with a resolvable route passes the route probe; ReadOnly then blocks with
    // permission_required. The config lives in its own directory so the workspace snapshot
    // taken for the Task does not drift when the file is rewritten.
    let routed_temp = tempfile::tempdir()?;
    let routed_root_config =
        sigil_kernel::RootConfig::load(&write_admission_test_config(routed_temp.path()))?;
    let mut readonly_config = routed_root_config.clone();
    readonly_config.permission.mode = sigil_kernel::PermissionMode::ReadOnly;
    let probes = crate::build_task_admission_probes(
        &readonly_config,
        temp.path(),
        None,
        &session,
        &receipt.task_id,
        &candidate,
    );
    assert!(probes.provider_route_available);
    assert!(!probes.permission_profile_ok);
    let outcome = crate::admit_adopted_task(
        &mut session,
        &readonly_config,
        temp.path(),
        &receipt.task_id,
        &candidate,
        &probes,
        41,
    )?;
    let sigil_kernel::TaskAdmissionOutcomeV1::Blocked(blocker) = &outcome else {
        panic!("read-only profile must block the task");
    };
    assert_eq!(
        blocker.reason_code,
        sigil_kernel::TaskBlockerReasonCodeV1::PermissionRequired
    );

    // Tiny free-disk probe blocks with disk_space_exhausted.
    let low_disk = crate::TaskAdmissionProbeContext {
        disk_space_bytes: Some(1024),
        provider_route_available: true,
        credential_available: true,
        permission_profile_ok: true,
        ..crate::TaskAdmissionProbeContext::default()
    };
    let outcome = crate::admit_adopted_task(
        &mut session,
        &routed_root_config,
        temp.path(),
        &receipt.task_id,
        &candidate,
        &low_disk,
        42,
    )?;
    let sigil_kernel::TaskAdmissionOutcomeV1::Blocked(blocker) = &outcome else {
        panic!("low disk must block the task");
    };
    assert_eq!(
        blocker.reason_code,
        sigil_kernel::TaskBlockerReasonCodeV1::DiskSpaceExhausted
    );
    Ok(())
}

#[test]
fn commit_draft_conflicts_on_candidate_and_marker_drift() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session = Session::new("mock", "model");
    let mut session = session.with_store(JsonlSessionStore::new(temp.path().join("spine.jsonl"))?);
    let request = PlanReviewRunRequest {
        plan_review_id: sigil_kernel::PlanReviewId::new("review-conflict")?,
        attempt_id: sigil_kernel::PlanReviewAttemptId::new("attempt-conflict")?,
        plan_id: sigil_kernel::PlanId::new("plan_spine_conflict")?,
        source: PlanReviewSource::ExplicitPlanCommand,
        source_turn: ConversationTurnRef {
            session_scope_id: "session-1".to_owned(),
            message_id: "message-1".to_owned(),
            logical_run_id: "run-1".to_owned(),
        },
        route_decision_id: None,
        child_session_ref: SessionRef::new_relative("child.jsonl")?,
        finalizer_session_ref: SessionRef::new_relative("finalizer.jsonl")?,
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        objective: "implement".to_owned(),
        workspace_snapshot_id: None,
    };
    let draft = draft_entry(&request);
    crate::PlanReviewCoordinator::ensure_attempt_started(&mut session, &request, 25)?;
    crate::PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        30,
    )?;
    // Same plan with a different compile input (different attempt provenance) produces a
    // different candidate: the existing candidate/marker must fail closed instead of being
    // silently overwritten.
    let mut drifted_input = test_plan_compile_input();
    drifted_input.source_attempt_id = "attempt-drifted".to_owned();
    drifted_input.task_config_contract_hash =
        sigil_kernel::stable_event_uuid("sigil-plan-task-config-v1", "drifted");
    let error = crate::PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &drifted_input,
        31,
    )
    .expect_err("candidate drift must fail closed");
    assert!(
        error
            .to_string()
            .contains("conflicting executable candidate"),
        "unexpected error: {error}"
    );
    // The durable facts are unchanged.
    let artifacts = session.plan_artifact_projection();
    assert_eq!(artifacts.candidates.len(), 1);
    assert_eq!(artifacts.ready_markers.len(), 1);
    assert_eq!(
        artifacts.plan_ready_state(&request.plan_id),
        sigil_kernel::PlanReadyStateV1::Ready
    );
    Ok(())
}

#[test]
fn durable_spine_records_validate_bounded_shapes() -> Result<()> {
    // Ready marker with a malformed candidate digest fails closed.
    let marker = sigil_kernel::PlanReadyCommittedV1Entry {
        plan_id: sigil_kernel::PlanId::new("plan_validate_1").expect("plan id"),
        plan_hash: "sha256:not-a-digest".to_owned(),
        candidate_hash: "sha256:also-not".to_owned(),
        attempt_id: "attempt-1".to_owned(),
        committed_at_ms: 10,
    };
    assert!(marker.validate().is_err());

    // Admission attempt with ordinal zero fails closed.
    let attempt = sigil_kernel::TaskAdmissionAttemptV1 {
        task_id: sigil_kernel::TaskId::new("plan-task-validate")?,
        plan_version: 1,
        ordinal: 0,
        candidate_hash: "sha256:bad".to_owned(),
        observed_environment: sigil_kernel::TaskAdmissionObservationV1 {
            base_workspace_snapshot_id: None,
            current_workspace_snapshot_id: None,
            workspace_state: sigil_kernel::WorkspaceAdmissionStateV1::ExactMatch,
            missing_capabilities: Vec::new(),
            provider_route_available: true,
            credential_available: true,
            permission_profile_ok: true,
            disk_space_bytes: None,
            external_writer_active: false,
            verification_runner_available: true,
            observed_at_ms: 1,
        },
        outcome: sigil_kernel::TaskAdmissionOutcomeV1::Ready(
            sigil_kernel::TaskRuntimeLeaseBindingV1 {
                lease_id: "lease-1".to_owned(),
                granted_at_ms: 1,
            },
        ),
    };
    assert!(attempt.validate().is_err());
    let mut valid = attempt;
    valid.ordinal = 1;
    valid.candidate_hash = format!("sha256:{}", "a".repeat(64));
    assert!(valid.validate().is_ok());
    Ok(())
}

#[test]
fn adopt_rejects_commands_bound_to_another_session() -> Result<()> {
    let (mut session, draft, _temp) = spine_session("plan_spine_scope", "Session scope")?;
    let candidate_hash = make_ready(&mut session, &draft)?;
    let parent_ref = SessionRef::new_relative("parent.jsonl")?;
    let command = sigil_kernel::PlanRunCommandV1 {
        command_id: "run-command-scope".to_owned(),
        session_id: "another-session".to_owned(),
        plan_id: draft.plan_id.clone(),
        expected_plan_hash: draft.plan_hash.clone(),
        expected_candidate_hash: candidate_hash.clone(),
        expected_durable_frontier: session.durable_frontier_sequence(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreateAndRun,
        permission: sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy,
        source: sigil_kernel::PlanRunCommandSource::Http,
    };
    let rejection = crate::PlanExecutionService::adopt(&mut session, parent_ref, &command, 30)
        .expect_err("cross-session command must be rejected");
    assert_eq!(rejection.reason_code(), "command_identity_conflict");
    assert!(session.plan_artifact_projection().adoptions.is_empty());
    Ok(())
}

#[test]
fn admission_probes_distinguish_route_shape_from_credential_availability() -> Result<()> {
    let temp = tempfile::tempdir()?;
    // Connection with an environment credential that is NOT set: route resolves, credential
    // must not be reported available.
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
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
base_url = "https://example.invalid"
credential = { source = "environment", name = "SIGIL_OPENAI_COMPATIBLE_API_KEY" }
"#,
    )?;
    let root_config = sigil_kernel::RootConfig::load(&config_path)?;
    let (session, draft, _temp) = spine_session("plan_spine_cred", "Credential probe")?;
    let base_snapshot = crate::plan_handoff_workspace_snapshot_id(&root_config, temp.path())?;
    let store_draft = sigil_kernel::PlanDraftCreatedEntry {
        workspace_snapshot_id: base_snapshot,
        ..draft
    };
    let candidate =
        sigil_kernel::compile_executable_plan_candidate(&store_draft, &test_plan_compile_input())
            .expect("fixture must compile");
    let task_id = candidate.task_id.clone();
    let probes = crate::build_task_admission_probes(
        &root_config,
        temp.path(),
        None,
        &session,
        &task_id,
        &candidate,
    );
    assert!(
        probes.provider_route_available,
        "a valid connection shape must resolve the route"
    );
    assert!(
        !probes.credential_available,
        "a missing environment credential must be discovered by the probe"
    );
    // With the variable set, the credential probe passes.
    let _scope = crate::test_env::EnvScope::set("SIGIL_OPENAI_COMPATIBLE_API_KEY", "test-key");
    let probes = crate::build_task_admission_probes(
        &root_config,
        temp.path(),
        None,
        &session,
        &task_id,
        &candidate,
    );
    assert!(probes.credential_available);
    Ok(())
}

#[test]
fn admission_permission_probe_considers_candidate_write_need() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut root_config =
        sigil_kernel::RootConfig::load(&write_admission_test_config(temp.path()))?;
    root_config.permission.mode = sigil_kernel::PermissionMode::ReadOnly;
    let (session, draft, _temp) = spine_session("plan_spine_perm", "Permission probe")?;
    let base_snapshot = crate::plan_handoff_workspace_snapshot_id(&root_config, temp.path())?;
    let store_draft = sigil_kernel::PlanDraftCreatedEntry {
        workspace_snapshot_id: base_snapshot,
        ..draft
    };
    // Write task under ReadOnly: blocked.
    let write_candidate =
        sigil_kernel::compile_executable_plan_candidate(&store_draft, &test_plan_compile_input())
            .expect("fixture must compile");
    let probes = crate::build_task_admission_probes(
        &root_config,
        temp.path(),
        None,
        &session,
        &write_candidate.task_id,
        &write_candidate,
    );
    assert!(!probes.permission_profile_ok);
    // Read-only task under ReadOnly: allowed.
    let mut read_draft = store_draft.clone();
    read_draft.steps[0].mode = Some(sigil_kernel::TaskStepMode::Read);
    read_draft.steps[0].isolation = Some(sigil_kernel::TaskIsolationMode::SharedReadOnly);
    read_draft.steps[0].required_capabilities = Vec::new();
    read_draft.plan_hash = sigil_kernel::plan_text_hash("Read-only probe plan");
    let read_candidate =
        sigil_kernel::compile_executable_plan_candidate(&read_draft, &test_plan_compile_input())
            .expect("read-only fixture must compile");
    assert!(
        read_candidate
            .required_capabilities
            .iter()
            .all(|cap| *cap != sigil_kernel::TaskCapabilityV2::WorkspaceWrite)
    );
    let probes = crate::build_task_admission_probes(
        &root_config,
        temp.path(),
        None,
        &session,
        &read_candidate.task_id,
        &read_candidate,
    );
    assert!(
        probes.permission_profile_ok,
        "a read-only task must not be blocked by read_only permission mode"
    );
    Ok(())
}

#[test]
fn admission_external_writer_probe_ignores_self_leases() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root_config = sigil_kernel::RootConfig::load(&write_admission_test_config(temp.path()))?;
    let (mut session, draft, _temp) = spine_session("plan_spine_lease", "Lease probe")?;
    let candidate =
        sigil_kernel::compile_executable_plan_candidate(&draft, &test_plan_compile_input())
            .expect("fixture must compile");
    let workspace_id = sigil_kernel::stable_workspace_id(temp.path())?;
    let task_id = candidate.task_id.clone();
    // A lease owned by this Task's own steps is not an external writer.
    let self_lease = sigil_kernel::WriteLeaseAcquired {
        lease_id: sigil_kernel::WriteLeaseId::new("lease-self")?,
        workspace_id: workspace_id.clone(),
        owner_agent_id: format!("task:{}:v1:step_1", task_id.as_str()),
        isolation_mode: sigil_kernel::WriteIsolationMode::SharedWorkspaceExclusive,
        scope: sigil_kernel::WriteLeaseScope::Workspace,
    };
    session.append_control(ControlEntry::WriteLeaseAcquired(self_lease))?;
    let probes = crate::build_task_admission_probes(
        &root_config,
        temp.path(),
        None,
        &session,
        &task_id,
        &candidate,
    );
    assert!(
        !probes.external_writer_active,
        "the task's own lease must not count as an external writer"
    );
    // A lease owned by another actor is an external writer.
    let other_lease = sigil_kernel::WriteLeaseAcquired {
        lease_id: sigil_kernel::WriteLeaseId::new("lease-other")?,
        workspace_id: workspace_id.clone(),
        owner_agent_id: "agent:other-thread".to_owned(),
        isolation_mode: sigil_kernel::WriteIsolationMode::SharedWorkspaceExclusive,
        scope: sigil_kernel::WriteLeaseScope::Workspace,
    };
    session.append_control(ControlEntry::WriteLeaseAcquired(other_lease))?;
    let probes = crate::build_task_admission_probes(
        &root_config,
        temp.path(),
        None,
        &session,
        &task_id,
        &candidate,
    );
    assert!(probes.external_writer_active);
    Ok(())
}

#[test]
fn admission_verification_probe_requires_runner_capability() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root_config = sigil_kernel::RootConfig::load(&write_admission_test_config(temp.path()))?;
    let (session, draft, _temp) = spine_session("plan_spine_verify", "Verify probe")?;
    let candidate =
        sigil_kernel::compile_executable_plan_candidate(&draft, &test_plan_compile_input())
            .expect("fixture must compile");
    let task_id = candidate.task_id.clone();
    // No registry evidence: falls back to the config policy only.
    let probes = crate::build_task_admission_probes(
        &root_config,
        temp.path(),
        None,
        &session,
        &task_id,
        &candidate,
    );
    assert!(probes.verification_runner_available);
    // auto_run = Never makes the runner unavailable regardless of the registry.
    let mut never_config = root_config.clone();
    never_config.verification.auto_run = sigil_kernel::VerificationAutoRunPolicy::Never;
    let probes = crate::build_task_admission_probes(
        &never_config,
        temp.path(),
        None,
        &session,
        &task_id,
        &candidate,
    );
    assert!(
        !probes.verification_runner_available,
        "auto_run=never must report the runner unavailable"
    );
    // Unresolvable workspace identity also makes the runner unavailable.
    let missing_workspace = temp.path().join("does-not-exist");
    let probes = crate::build_task_admission_probes(
        &root_config,
        &missing_workspace,
        None,
        &session,
        &task_id,
        &candidate,
    );
    assert!(
        !probes.verification_runner_available,
        "an unresolvable workspace must report the runner unavailable"
    );
    Ok(())
}

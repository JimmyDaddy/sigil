use std::{
    pin::Pin,
    sync::{Arc, Mutex},
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures::{Stream, stream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener as TokioTcpListener;

use sigil_kernel::{
    Agent, AgentRunOptions, AgentRunPurpose, AutoApproveHandler, CompactionConfig,
    CompletionRequest, ControlEntry, ConversationRoute, ConversationRouteDecisionRecordedEntry,
    ConversationTurnRef, EventHandler, InteractionMode, JsonlSessionStore, MemoryConfig,
    ModelMessage, NoopEventHandler, PermissionConfig, PermissionEvaluationContext, PlanDecision,
    PlanDraftCreatedEntry, PlanId, PlanReviewAttemptStatus, PlanReviewProjection, PlanReviewSource,
    PlanSourceRef, Provider, ProviderCapabilities, ProviderChunk, RunCancellationOwner, RunEvent,
    Session, SessionLogEntry, SessionRef, TaskRoutingPolicy, TaskRunStatus, Tool, ToolAccess,
    ToolCall, ToolCategory, ToolContext, ToolPreviewCapability, ToolRegistry, ToolResult,
    ToolResultMeta, ToolSpec, conversation_route_decision_id_for_source,
    plan_review_attempt_id_for_review, plan_review_plan_id_for_attempt,
    plan_review_policy_snapshot_hash,
};

use crate::PlanReviewRunOutcome;
use crate::{
    ConversationCoordinator, PlanDecisionCommand, PlanReviewCoordinator, PlanReviewRunRequest,
};

fn session_with_route_decision() -> Result<(Session, PlanReviewRunRequest)> {
    let mut session = Session::new("plan-review-test", "planned-model");
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
        objective: prompt.to_owned(),
        workspace_snapshot_id: None,
    };
    Ok((session, request))
}

fn draft_entry(plan_id: &PlanId) -> PlanDraftCreatedEntry {
    PlanDraftCreatedEntry {
        plan_id: plan_id.clone(),
        schema_version: 2,
        source: PlanSourceRef::default(),
        plan_hash: "sha256:draft".to_owned(),
        summary: "Migrate the coordinator".to_owned(),
        inline_text: None,
        steps: Vec::new(),
        intent_proposal: None,
        target_paths: vec!["src/coordinator.rs".to_owned()],
        suggested_checks: Vec::new(),
        risk: None,
        notes: Vec::new(),
        workspace_snapshot_id: None,
        created_at_ms: 50,
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

#[tokio::test]
async fn plan_review_research_is_bounded_and_finalizes_with_only_the_submit_tool() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (parent_session, request) = session_with_route_decision()?;
    let parent_session = parent_session.with_store(JsonlSessionStore::new(
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
        &parent_session,
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
async fn plan_review_stream_interruption_uses_durable_evidence_for_submit_only_finalization()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let (parent_session, request) = session_with_route_decision()?;
    let parent_session = parent_session.with_store(JsonlSessionStore::new(
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
        &parent_session,
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
    let parent_session = parent_session.with_store(JsonlSessionStore::new(
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
        &parent_session,
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
) -> Result<crate::ApplicationPlanDecisionReceipt> {
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
    let request =
        PlanReviewCoordinator::prepare_automatic_plan_review(&mut session, &action, None, 100)?;
    let mut draft = draft_entry(&request.plan_id);
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
        suggested_checks: Vec::new(),
        risk: None,
        notes: Vec::new(),
    }];
    draft.workspace_snapshot_id = Some(
        crate::plan_handoff_workspace_snapshot_id(root_config, workspace_root)?
            .expect("workspace snapshot"),
    );
    PlanReviewCoordinator::commit_draft_from_child(&mut session, &draft, &request, 110)?;
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
    Ok(receipt)
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
    let receipt = seed_revision_decision(&root_config, &workspace_root, &session_path)?;
    assert!(
        receipt.revision_request.is_some(),
        "Revise must return the prepared revision run"
    );

    // Executing the revision runs a real plan review and commits the new draft.
    let mut recorded = Vec::new();
    let mut handler = RecordingRevisionHandler(std::mem::take(&mut recorded));
    let outcome = crate::application_run::execute_plan_review_revision(
        &root_config,
        &workspace_root,
        &session_path,
        &receipt.revision_request.clone().expect("revision request"),
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
    let receipt = seed_revision_decision(&root_config, &workspace_root, &session_path)?;
    let revision_request = receipt.revision_request.clone().expect("revision request");

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
    let receipt = seed_revision_decision(&root_config, &workspace_root, &session_path)?;
    let revision_request = receipt.revision_request.clone().expect("revision request");

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
    let mut conflicting = draft_entry(&revision_request.plan_id);
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
    let draft = draft_entry(&request.plan_id);

    PlanReviewCoordinator::commit_draft_from_child(&mut session, &draft, &request, 110)?;
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
    PlanReviewCoordinator::commit_draft_from_child(&mut session, &draft, &request, 111)?;
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
        PlanReviewCoordinator::commit_draft_from_child(&mut session, &conflicting, &request, 112)
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
    let draft = draft_entry(&request.plan_id);
    PlanReviewCoordinator::commit_draft_from_child(&mut session, &draft, &request, 110)?;

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
fn create_task_from_plan_promotes_valid_draft_and_is_idempotent() -> Result<()> {
    let (mut session, request) = session_with_route_decision()?;
    let action = sigil_kernel::StartPlanReviewAction {
        decision_id: request.route_decision_id.clone().expect("decision id"),
        plan_review_id: request.plan_review_id.clone(),
        plan_id: request.plan_id.clone(),
        source_turn: request.source_turn.clone(),
    };
    PlanReviewCoordinator::prepare_automatic_plan_review(&mut session, &action, None, 100)?;
    let draft = draft_entry(&request.plan_id);
    PlanReviewCoordinator::commit_draft_from_child(&mut session, &draft, &request, 110)?;

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
    let create = crate::CreateTaskFromPlanRequest {
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
    let (mut bound_session, bound_request) = session_with_route_decision()?;
    let bound_action = sigil_kernel::StartPlanReviewAction {
        decision_id: bound_request
            .route_decision_id
            .clone()
            .expect("decision id"),
        plan_review_id: bound_request.plan_review_id.clone(),
        plan_id: bound_request.plan_id.clone(),
        source_turn: bound_request.source_turn.clone(),
    };
    PlanReviewCoordinator::prepare_automatic_plan_review(
        &mut bound_session,
        &bound_action,
        None,
        100,
    )?;
    let mut bound_draft = draft_entry(&bound_request.plan_id);
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
        suggested_checks: Vec::new(),
        risk: None,
        notes: Vec::new(),
    }];
    let snapshot = crate::plan_handoff_workspace_snapshot_id(&root_config, temp.path())?
        .expect("workspace snapshot");
    bound_draft.workspace_snapshot_id = Some(snapshot);
    PlanReviewCoordinator::commit_draft_from_child(
        &mut bound_session,
        &bound_draft,
        &bound_request,
        110,
    )?;
    let bound_create = crate::CreateTaskFromPlanRequest {
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
    let (mut drift_session, drift_request) = session_with_route_decision()?;
    let drift_action = sigil_kernel::StartPlanReviewAction {
        decision_id: drift_request
            .route_decision_id
            .clone()
            .expect("decision id"),
        plan_review_id: drift_request.plan_review_id.clone(),
        plan_id: drift_request.plan_id.clone(),
        source_turn: drift_request.source_turn.clone(),
    };
    PlanReviewCoordinator::prepare_automatic_plan_review(
        &mut drift_session,
        &drift_action,
        None,
        100,
    )?;
    let mut drift_draft = draft_entry(&drift_request.plan_id);
    drift_draft.steps = bound_draft.steps.clone();
    let drift_snapshot = crate::plan_handoff_workspace_snapshot_id(&root_config, temp.path())?
        .expect("workspace snapshot");
    drift_draft.workspace_snapshot_id = Some(drift_snapshot);
    PlanReviewCoordinator::commit_draft_from_child(
        &mut drift_session,
        &drift_draft,
        &drift_request,
        110,
    )?;
    let changed_root = tempfile::tempdir()?;
    std::fs::write(changed_root.path().join("marker.txt"), b"changed")?;
    let drift_create = crate::CreateTaskFromPlanRequest {
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
    let stale = crate::CreateTaskFromPlanRequest {
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
fn reject_plan_is_durable_and_prevents_task_creation() -> Result<()> {
    let (mut session, request) = session_with_route_decision()?;
    let action = sigil_kernel::StartPlanReviewAction {
        decision_id: request.route_decision_id.clone().expect("decision id"),
        plan_review_id: request.plan_review_id.clone(),
        plan_id: request.plan_id.clone(),
        source_turn: request.source_turn.clone(),
    };
    PlanReviewCoordinator::prepare_automatic_plan_review(&mut session, &action, None, 100)?;
    let draft = draft_entry(&request.plan_id);
    PlanReviewCoordinator::commit_draft_from_child(&mut session, &draft, &request, 110)?;

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

    let create = crate::CreateTaskFromPlanRequest {
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

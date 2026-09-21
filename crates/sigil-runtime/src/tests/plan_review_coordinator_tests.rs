use std::{
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use futures::{Stream, stream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener as TokioTcpListener;

use sigil_kernel::{
    Agent, AgentRunOptions, AgentRunPurpose, AutoApproveHandler, CompactionConfig,
    CompletionRequest, ControlEntry, ConversationRoute, ConversationRouteDecisionRecordedEntry,
    ConversationTurnRef, DeclaredToolPermissionFacts, EventHandler, InteractionMode,
    JsonlSessionStore, MemoryConfig, ModelMessage, NetworkEffect, NetworkPolicy, NoopEventHandler,
    PermissionConfig, PermissionEvaluationContext, PlanDecision, PlanDraftCreatedEntry,
    PlanReviewAttemptStatus, PlanReviewProjection, PlanReviewSource, Provider,
    ProviderCapabilities, ProviderChunk, PublicRunEventKind, RunCancellationOwner, RunEvent,
    Session, SessionLogEntry, SessionRef, TaskRoutingPolicy, Tool, ToolAccess, ToolCall,
    ToolCategory, ToolContext, ToolErrorKind, ToolOperation, ToolPermissionPlanDraft,
    ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta, ToolSpec,
    conversation_route_decision_id_for_source, declared_tool_permission_plan,
    plan_review_attempt_id_for_review, plan_review_plan_id_for_attempt,
    plan_review_policy_snapshot_hash,
};

use crate::PlanReviewRunOutcome;
use crate::{
    ConversationCoordinator, PlanDecisionCommand, PlanReviewCoordinator, PlanReviewRunRequest,
};

#[path = "plan_review_submission_mixed_tests.rs"]
mod plan_review_submission_mixed_tests;

#[path = "plan_review_application_operation_tests.rs"]
mod application_operation;

fn isolated_storage_toml(path: &std::path::Path) -> String {
    let root = path.parent().expect("test config should have a parent");
    let state_root = toml::Value::String(root.join("state").to_string_lossy().into_owned());
    let cache_root = toml::Value::String(root.join("cache").to_string_lossy().into_owned());
    format!("[storage]\nstate_root = {state_root}\ncache_root = {cache_root}\n")
}

#[test]
fn current_schema_child_resource_bundle_is_scoped_and_explicitly_finalized() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;

    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let execution_temp = temp.path().join("execution-temp");
    std::fs::create_dir_all(state.join("cache"))?;
    std::fs::create_dir_all(&execution_temp)?;
    let composition = crate::r71_authority_composition::compose_runtime_authority(
        &state,
        &execution_temp,
        sigil_kernel::resource::CanonicalHash::from_bytes([0x91; 32]),
        sigil_kernel::resource::AuthorityGeneration {
            epoch: 1,
            instance_hash: sigil_kernel::resource::CanonicalHash::from_bytes([0x75; 32]),
        },
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        )),
        &[
            Channel::SessionLog,
            Channel::ArtifactStaging,
            Channel::ArtifactStore,
        ],
    )?;
    let provisioner = composition.plan_review_child_resource_provisioner();
    let (_, request) = session_with_route_decision()?;

    let bundle = provisioner.provision(&request)?;
    assert!(!bundle.scope_id().is_empty());
    assert_eq!(
        bundle.authority_generation(),
        composition.authority_generation()
    );
    assert!(!format!("{bundle:?}").contains("state/"));
    bundle.finish()?;

    // Explicit finish settles both the child SessionLog and paired artifact namespaces. Reusing
    // the same deterministic child key is the recovery assertion: a leaked pending lease would
    // poison the next admission instead of closing the child scope.
    let recovered = provisioner.provision(&request)?;
    recovered.finish()?;
    Ok(())
}

fn child_resource_fixture(
    channels: &[crate::managed_storage_writer::StorageWriterChannelV1],
) -> Result<(
    tempfile::TempDir,
    Arc<dyn crate::plan_review_coordinator::PlanReviewChildResourceProvisionerV1>,
    PlanReviewRunRequest,
)> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let execution_temp = temp.path().join("execution-temp");
    std::fs::create_dir_all(state.join("cache"))?;
    std::fs::create_dir_all(&execution_temp)?;
    let composition = crate::r71_authority_composition::compose_runtime_authority(
        &state,
        &execution_temp,
        sigil_kernel::resource::CanonicalHash::from_bytes([0x91; 32]),
        sigil_kernel::resource::AuthorityGeneration {
            epoch: 1,
            instance_hash: sigil_kernel::resource::CanonicalHash::from_bytes([0x75; 32]),
        },
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        )),
        channels,
    )?;
    let provisioner = composition.plan_review_child_resource_provisioner();
    let (_, request) = session_with_route_decision()?;
    Ok((temp, provisioner, request))
}

#[test]
fn r71_f_csr_001_research_bundle_has_a_scoped_identity() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;
    let (_temp, provisioner, request) = child_resource_fixture(&[
        Channel::SessionLog,
        Channel::ArtifactStaging,
        Channel::ArtifactStore,
    ])?;
    let bundle = provisioner.provision(&request)?;
    assert!(
        bundle
            .scope_id()
            .starts_with(&request.child_logical_run_id())
    );
    assert!(bundle.scope_id().ends_with("-research"));
    bundle.finish()?;
    Ok(())
}

#[test]
fn r71_f_csr_002_bundle_carries_current_authority_generation() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;
    let (_temp, provisioner, request) = child_resource_fixture(&[
        Channel::SessionLog,
        Channel::ArtifactStaging,
        Channel::ArtifactStore,
    ])?;
    let bundle = provisioner.provision(&request)?;
    assert_eq!(bundle.authority_generation().epoch, 1);
    assert_eq!(
        bundle.authority_generation().instance_hash,
        sigil_kernel::resource::CanonicalHash::from_bytes([0x75; 32])
    );
    bundle.finish()?;
    Ok(())
}

#[test]
fn r71_f_csr_003_bundle_debug_never_exposes_physical_paths() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;
    let (temp, provisioner, request) = child_resource_fixture(&[
        Channel::SessionLog,
        Channel::ArtifactStaging,
        Channel::ArtifactStore,
    ])?;
    let bundle = provisioner.provision(&request)?;
    let debug = format!("{bundle:?}");
    assert!(!debug.contains(temp.path().to_string_lossy().as_ref()));
    assert!(!debug.contains("records.jsonl"));
    bundle.finish()?;
    Ok(())
}

#[test]
fn r71_f_csr_004_research_finish_releases_the_exact_admissions() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;
    let (_temp, provisioner, request) = child_resource_fixture(&[
        Channel::SessionLog,
        Channel::ArtifactStaging,
        Channel::ArtifactStore,
    ])?;
    let bundle = provisioner.provision(&request)?;
    bundle.finish()?;
    let recovered = provisioner.provision(&request)?;
    recovered.finish()?;
    Ok(())
}

#[test]
fn r71_f_csr_006_different_attempts_never_share_child_scope() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;
    let (_temp, provisioner, request) = child_resource_fixture(&[
        Channel::SessionLog,
        Channel::ArtifactStaging,
        Channel::ArtifactStore,
    ])?;
    let first = provisioner.provision(&request)?;
    let mut next_request = request.clone();
    next_request.attempt_id = sigil_kernel::PlanReviewAttemptId::new("next-research-attempt")?;
    next_request.child_session_ref = sigil_kernel::plan_review_child_session_ref(
        &next_request.plan_review_id,
        &next_request.attempt_id,
    );
    let next = provisioner.provision(&next_request)?;
    assert_ne!(first.scope_id(), next.scope_id());
    assert_ne!(first.session_log_path(), next.session_log_path());
    next.finish()?;
    first.finish()?;
    Ok(())
}

#[test]
fn r71_f_csr_007_missing_artifact_authority_fails_before_child_bundle() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;
    let (_temp, provisioner, request) = child_resource_fixture(&[Channel::SessionLog])?;
    let error = provisioner
        .provision(&request)
        .expect_err("missing artifact authority");
    assert!(error.to_string().contains("artifact-staging"));
    Ok(())
}

#[test]
fn managed_child_partial_artifact_admission_settles_session_log() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;

    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let execution_temp = temp.path().join("execution-temp");
    std::fs::create_dir_all(state.join("cache"))?;
    std::fs::create_dir_all(&execution_temp)?;
    let composition = crate::r71_authority_composition::compose_runtime_authority(
        &state,
        &execution_temp,
        sigil_kernel::resource::CanonicalHash::from_bytes([0x91; 32]),
        sigil_kernel::resource::AuthorityGeneration {
            epoch: 1,
            instance_hash: sigil_kernel::resource::CanonicalHash::from_bytes([0x75; 32]),
        },
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        )),
        &[
            Channel::SessionLog,
            Channel::ArtifactStaging,
            Channel::ArtifactStore,
        ],
    )?;
    let (_, request) = session_with_route_decision()?;
    let key = format!("pr-{}-research-0", request.attempt_id.as_str(),);
    let staging_path = composition
        .storage_writer
        .managed_named_leaf_path(Channel::ArtifactStaging, &key)?;
    std::fs::create_dir_all(staging_path.parent().context("staging parent")?)?;
    // Force the second-stage artifact admission to fail after SessionLog was admitted. A regular
    // file is portable across Unix/Windows and is rejected before the authority can mutate it.
    std::fs::write(&staging_path, b"not-a-directory")?;

    let provisioner = composition.plan_review_child_resource_provisioner();
    let error = provisioner
        .provision(&request)
        .expect_err("artifact admission should fail on the occupied staging leaf");
    assert!(error.to_string().contains("artifact"));

    // If the partial bundle path relied on Drop, this exact retry would still be blocked by the
    // first SessionLog admission. Successful re-admission proves the explicit settlement ran.
    let session_lease = composition
        .storage_writer
        .acquire_named(Channel::SessionLog, &key)?;
    composition.storage_writer.finalize(session_lease)?;
    Ok(())
}

#[test]
fn r71_f_csr_008_child_scope_is_retry_stable() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;
    let (_temp, provisioner, request) = child_resource_fixture(&[
        Channel::SessionLog,
        Channel::ArtifactStaging,
        Channel::ArtifactStore,
    ])?;
    let first = provisioner.provision(&request)?;
    let first_scope = first.scope_id().to_owned();
    first.finish()?;
    let retry = provisioner.provision(&request)?;
    assert_eq!(retry.scope_id(), first_scope);
    retry.finish()?;
    Ok(())
}

fn session_with_route_decision() -> Result<(Session, PlanReviewRunRequest)> {
    seed_route_decision(Session::new("plan-review-test", "planned-model"))
}

fn seed_route_decision(session: Session) -> Result<(Session, PlanReviewRunRequest)> {
    seed_route_decision_with_prior_context(session, false)
}

fn seed_route_decision_with_prior_context(
    session: Session,
    include_prior_context: bool,
) -> Result<(Session, PlanReviewRunRequest)> {
    seed_route_decision_with_progress(session, include_prior_context, false)
}

fn seed_route_decision_with_progress(
    mut session: Session,
    include_prior_context: bool,
    include_progress: bool,
) -> Result<(Session, PlanReviewRunRequest)> {
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "plan-review-test".to_owned(),
        model_name: "planned-model".to_owned(),
        resolved_model_route: None,
    })?;
    if include_prior_context {
        let mut prior_user = ModelMessage::user("preserve this earlier migration constraint");
        prior_user.id = "user-prior".to_owned();
        session.append_user_message(prior_user)?;
        let mut prior_assistant = ModelMessage::assistant(
            Some("Earlier discussion established the migration must remain reversible.".to_owned()),
            Vec::new(),
        );
        prior_assistant.id = "assistant-prior".to_owned();
        session.append_assistant_message(prior_assistant)?;
    }
    let prompt = "design the migration before touching anything";
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        "user-1".to_owned(),
        "plan-review-run",
    )?;
    let mut message = ModelMessage::user(prompt);
    message.id = "user-1".to_owned();
    session.append_user_message(message)?;
    if include_progress {
        session.append_assistant_message(ModelMessage::assistant(
            Some("already inspected migration dependencies before planning".to_owned()),
            Vec::new(),
        ))?;
    }
    let handoff_audit = if include_progress {
        let call = ToolCall {
            id: "review-handoff".to_owned(),
            name: sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
            args_json: serde_json::json!({"reason_codes":["architectural_tradeoff"]}).to_string(),
        };
        let mut declaration = ModelMessage::assistant(None, vec![call]);
        declaration.logical_run_id = Some(sigil_kernel::LogicalRunId::new("plan-review-run")?);
        session.append_assistant_message(declaration)?;
        let audit = sigil_kernel::ToolExecutionEntry {
            call_id: "review-handoff".to_owned(),
            tool_name: sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
            status: sigil_kernel::ToolExecutionStatus::Started,
            duration_ms: None,
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: None,
        };
        session.append_control(ControlEntry::ToolExecution(Box::new(audit.clone())))?;
        Some(audit)
    } else {
        None
    };
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
    if let Some(mut audit) = handoff_audit {
        audit.status = sigil_kernel::ToolExecutionStatus::Completed;
        session.append_control(ControlEntry::ToolExecution(Box::new(audit)))?;
        let result = ToolResult::ok(
            "review-handoff",
            sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME,
            "accepted",
            ToolResultMeta::default(),
        );
        let (recorded, _) = sigil_kernel::ToolResultRecordedV3::capture(
            &result,
            None,
            sigil_kernel::ToolArtifactSensitivity::Ordinary,
        )?;
        session.append(SessionLogEntry::ToolResultV3(recorded))?;
    }
    let plan_review_id = sigil_kernel::plan_review_id_for_source(&source_turn);
    let attempt_id = plan_review_attempt_id_for_review(&plan_review_id);
    let plan_id = plan_review_plan_id_for_attempt(&plan_review_id, &attempt_id);
    let request = PlanReviewRunRequest {
        application_operation: None,
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
        revision_request_id: None,
        revision_generation: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        explicit_objective: None,
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

fn ensure_test_plan_review_attempt_started(
    session: &mut Session,
    request: &PlanReviewRunRequest,
    now_ms: u64,
) -> Result<()> {
    if request.revision_request_id.is_some() {
        PlanReviewCoordinator::ensure_revision_attempt_started(session, request, now_ms)
    } else {
        PlanReviewCoordinator::ensure_attempt_started(
            session,
            request,
            &mut NoopEventHandler,
            now_ms,
        )
    }
}

fn commit_test_plan_review_draft(
    session: &mut Session,
    draft: &PlanDraftCreatedEntry,
    request: &PlanReviewRunRequest,
    now_ms: u64,
) -> Result<()> {
    if PlanReviewProjection::from_entries(session.entries())
        .latest_attempt(&request.plan_review_id)
        .is_none()
    {
        ensure_test_plan_review_attempt_started(session, request, now_ms)?;
    }
    PlanReviewCoordinator::commit_draft_from_child(
        session,
        draft,
        request,
        &mut NoopEventHandler,
        now_ms,
    )
}

fn close_test_plan_review_run(
    session: &mut Session,
    request: &PlanReviewRunRequest,
    outcome: &PlanReviewRunOutcome,
    now_ms: u64,
) -> Result<()> {
    if PlanReviewProjection::from_entries(session.entries())
        .latest_attempt(&request.plan_review_id)
        .is_none()
    {
        ensure_test_plan_review_attempt_started(session, request, now_ms)?;
    }
    PlanReviewCoordinator::close_plan_review_run(
        session,
        request,
        outcome,
        &mut NoopEventHandler,
        now_ms,
    )
}

fn close_test_plan_review_run_if_open(
    session: &mut Session,
    request: &PlanReviewRunRequest,
    outcome: &PlanReviewRunOutcome,
    now_ms: u64,
) -> Result<()> {
    PlanReviewCoordinator::close_plan_review_run_if_open(
        session,
        request,
        outcome,
        &mut NoopEventHandler,
        now_ms,
    )
}

fn complete_test_plan_review_without_draft(
    session: &mut Session,
    request: &PlanReviewRunRequest,
    now_ms: u64,
) -> Result<()> {
    if PlanReviewProjection::from_entries(session.entries())
        .latest_attempt(&request.plan_review_id)
        .is_none()
    {
        ensure_test_plan_review_attempt_started(session, request, now_ms)?;
    }
    PlanReviewCoordinator::complete_without_draft(session, request, &mut NoopEventHandler, now_ms)
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
        tool_authority: None,
    }
}

fn submitted_draft_chunks(call_id: &str) -> Vec<Result<ProviderChunk>> {
    let args = r##"{
        "schema_version": 1,
        "outcome": "draft",
        "content": "# Bounded plan review\n\n1. Implement the bounded change."
    }"##;
    vec![
        Ok(ProviderChunk::ToolCallStart {
            id: call_id.to_owned(),
            name: sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned(),
        }),
        Ok(ProviderChunk::ToolCallArgsDelta {
            id: call_id.to_owned(),
            delta: args.to_owned(),
        }),
        Ok(ProviderChunk::ToolCallComplete(ToolCall {
            id: call_id.to_owned(),
            name: sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned(),
            args_json: args.to_owned(),
        })),
        Ok(ProviderChunk::Done),
    ]
}

fn invalid_draft_chunks(call_id: &str) -> Vec<Result<ProviderChunk>> {
    let args = r#"{"schema_version":1,"outcome":"draft","content":""}"#;
    vec![
        Ok(ProviderChunk::ToolCallStart {
            id: call_id.to_owned(),
            name: sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned(),
        }),
        Ok(ProviderChunk::ToolCallArgsDelta {
            id: call_id.to_owned(),
            delta: args.to_owned(),
        }),
        Ok(ProviderChunk::ToolCallComplete(ToolCall {
            id: call_id.to_owned(),
            name: sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned(),
            args_json: args.to_owned(),
        })),
        Ok(ProviderChunk::Done),
    ]
}

fn no_plan_chunks(call_id: &str, reason: &str) -> Vec<Result<ProviderChunk>> {
    let args = serde_json::json!({
        "schema_version": 1, "outcome": "no_plan", "content": reason,
    })
    .to_string();
    vec![
        Ok(ProviderChunk::ToolCallStart {
            id: call_id.to_owned(),
            name: sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned(),
        }),
        Ok(ProviderChunk::ToolCallArgsDelta {
            id: call_id.to_owned(),
            delta: args.to_owned(),
        }),
        Ok(ProviderChunk::ToolCallComplete(ToolCall {
            id: call_id.to_owned(),
            name: sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned(),
            args_json: args.to_owned(),
        })),
        Ok(ProviderChunk::Done),
    ]
}

struct PlanReviewInspectionTool;

#[derive(Clone, Copy)]
enum PlanReviewEffectProbeKind {
    WebRead,
    TrustedMcpRead,
    WorkspaceWrite,
}

struct PlanReviewEffectProbeTool {
    kind: PlanReviewEffectProbeKind,
    executions: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct PlanReviewEffectProbeProvider {
    tool_name: String,
    requested_tool_surfaces: Arc<Mutex<Vec<Vec<String>>>>,
    observed_permission_error: Arc<AtomicBool>,
}

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

#[async_trait]
impl Tool for PlanReviewEffectProbeTool {
    fn spec(&self) -> ToolSpec {
        let (name, category, access, network_effect) = match self.kind {
            PlanReviewEffectProbeKind::WebRead => (
                "websearch",
                ToolCategory::Search,
                ToolAccess::Read,
                Some(NetworkEffect::Read),
            ),
            PlanReviewEffectProbeKind::TrustedMcpRead => (
                "mcp__trusted__search",
                ToolCategory::Mcp,
                ToolAccess::Read,
                Some(NetworkEffect::Read),
            ),
            PlanReviewEffectProbeKind::WorkspaceWrite => {
                ("write_file", ToolCategory::File, ToolAccess::Write, None)
            }
        };
        ToolSpec {
            name: name.to_owned(),
            description: "Permission-probed PlanReview capability".to_owned(),
            input_schema: serde_json::json!({"type":"object","properties":{}}),
            category,
            access,
            network_effect,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_plan(
        &self,
        _ctx: &ToolContext,
        args: &serde_json::Value,
    ) -> Result<ToolPermissionPlanDraft> {
        let spec = self.spec();
        match self.kind {
            PlanReviewEffectProbeKind::WebRead => declared_tool_permission_plan(
                &spec,
                args,
                DeclaredToolPermissionFacts {
                    access: ToolAccess::Read,
                    operation: ToolOperation::NetworkRequest,
                    network_effect: Some(NetworkEffect::Read),
                    subjects: Vec::new(),
                    tool_default_mode: None,
                    managed_file_access: None,
                },
            ),
            PlanReviewEffectProbeKind::TrustedMcpRead => sigil_mcp::mcp_tool_permission_plan(
                &spec.name,
                "search",
                &sigil_mcp::McpToolAnnotations {
                    read_only_hint: Some(true),
                    destructive_hint: Some(false),
                    idempotent_hint: Some(false),
                    open_world_hint: Some(true),
                    ..sigil_mcp::McpToolAnnotations::default()
                },
                &sigil_kernel::McpServerTrustPolicy {
                    trust_class: sigil_kernel::McpTrustClass::SelfHosted,
                    approval_default: sigil_kernel::ApprovalMode::Allow,
                    ..sigil_kernel::McpServerTrustPolicy::default()
                },
                sigil_mcp::McpPermissionTransport::StreamableHttp,
                Vec::new(),
                &sigil_mcp::McpPermissionBinding {
                    execution_profile: "sha256:trusted-fixture".to_owned(),
                    environment_binding: "sha256:trusted-fixture-env".to_owned(),
                },
            ),
            PlanReviewEffectProbeKind::WorkspaceWrite => declared_tool_permission_plan(
                &spec,
                args,
                DeclaredToolPermissionFacts {
                    access: ToolAccess::Write,
                    operation: ToolOperation::OverwriteFile,
                    network_effect: None,
                    subjects: Vec::new(),
                    tool_default_mode: None,
                    managed_file_access: None,
                },
            ),
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::ok(
            call_id,
            self.spec().name,
            "effect probe executed",
            ToolResultMeta::default(),
        ))
    }
}

#[async_trait]
impl Provider for PlanReviewEffectProbeProvider {
    fn name(&self) -> &str {
        "plan-review-effect-probe"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        plan_review_provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let index = {
            let mut surfaces = self
                .requested_tool_surfaces
                .lock()
                .map_err(|_| anyhow!("PlanReview effect probe surface lock poisoned"))?;
            let index = surfaces.len();
            surfaces.push(request.tools.iter().map(|tool| tool.name.clone()).collect());
            index
        };
        if index == 0 {
            let call_id = "plan-review-effect-probe".to_owned();
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: call_id.clone(),
                    name: self.tool_name.clone(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: call_id.clone(),
                    delta: "{}".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: call_id,
                    name: self.tool_name.clone(),
                    args_json: "{}".to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }

        if request.messages.iter().any(|message| {
            message.tool_result_payload().is_some_and(|payload| {
                payload.wire_semantics.error_kind == Some(ToolErrorKind::PermissionDenied)
            })
        }) {
            self.observed_permission_error.store(true, Ordering::SeqCst);
        }
        Ok(Box::pin(stream::iter(no_plan_chunks(
            "effect-probe-no-plan",
            "The effect probe completed its permission check.",
        ))))
    }
}

#[derive(Clone, Default)]
struct LoopingPlanReviewProvider {
    request_tools: Arc<Mutex<Vec<Vec<String>>>>,
    request_messages: Arc<Mutex<Vec<Vec<ModelMessage>>>>,
    research_turns: Option<usize>,
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
        self.request_messages
            .lock()
            .map_err(|_| anyhow!("plan review message recorder lock poisoned"))?
            .push(request.messages.clone());
        if tool_names.iter().any(|name| name == "inspect_workspace")
            && request_index < self.research_turns.unwrap_or(8)
        {
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
            .any(|name| name == sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME)
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
                .any(|name| name == sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME)
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
struct UnexpectedToolCallProvider {
    request_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[derive(Clone, Default)]
struct InvalidThenValidSubmissionProvider {
    request_tools: Arc<Mutex<Vec<Vec<String>>>>,
    request_messages: Arc<Mutex<Vec<Vec<ModelMessage>>>>,
    interrupt_research: bool,
    research_text: Option<String>,
}

#[derive(Default)]
struct InvalidThenCorrectedDraftProvider {
    calls: AtomicUsize,
    request_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[derive(Clone, Default)]
struct NoPlanPlanReviewProvider {
    request_tools: Arc<Mutex<Vec<Vec<String>>>>,
    reason: Option<String>,
}

#[derive(Clone, Default)]
struct PlainTextOnlyReviewProvider {
    request_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[derive(Default)]
struct ResearchThenDraftProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for ResearchThenDraftProvider {
    fn name(&self) -> &str {
        "research-then-draft-plan-review"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        plan_review_provider_capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(call, 0, "a submitted draft must not trigger redispatch");
        let mut chunks = vec![Ok(ProviderChunk::TextDelta("research complete".to_owned()))];
        chunks.extend(submitted_draft_chunks("model-submitted-draft"));
        Ok(Box::pin(stream::iter(chunks)))
    }
}

#[async_trait]
impl Provider for PlainTextOnlyReviewProvider {
    fn name(&self) -> &str {
        "plain-text-plan-review"
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
        let text = if index == 0 {
            "The workspace inspection is complete."
        } else {
            "# Implementation plan\n\n1. Inspect the live call path.\n2. Apply the smallest safe change.\n3. Run the relevant checks."
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(text.to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for InvalidThenValidSubmissionProvider {
    fn name(&self) -> &str {
        "invalid-then-valid-plan-review"
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
        self.request_messages
            .lock()
            .map_err(|_| anyhow!("plan review message recorder lock poisoned"))?
            .push(request.messages.clone());
        Ok(Box::pin(stream::iter(match index {
            0 if self.interrupt_research => vec![
                Ok(ProviderChunk::TextDelta(
                    "partial research response".to_owned(),
                )),
                Err(anyhow!("simulated TLS unexpected EOF")),
            ],
            0 => vec![
                Ok(ProviderChunk::TextDelta(
                    self.research_text
                        .clone()
                        .unwrap_or_else(|| "research complete".to_owned()),
                )),
                Ok(ProviderChunk::Done),
            ],
            1 => invalid_draft_chunks("invalid-draft"),
            _ => submitted_draft_chunks("corrected-draft"),
        })))
    }
}

#[async_trait]
impl Provider for InvalidThenCorrectedDraftProvider {
    fn name(&self) -> &str {
        "invalid-then-corrected-plan-review-draft"
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
            invalid_draft_chunks("invalid-plan-draft")
        } else {
            submitted_draft_chunks("corrected-plan-draft")
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}

#[async_trait]
impl Provider for NoPlanPlanReviewProvider {
    fn name(&self) -> &str {
        "no-plan-plan-review"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        plan_review_provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let mut requests = self
            .request_tools
            .lock()
            .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
        requests.push(request.tools.iter().map(|tool| tool.name.clone()).collect());
        drop(requests);
        Ok(Box::pin(stream::iter(no_plan_chunks(
            "no-plan",
            self.reason
                .as_deref()
                .unwrap_or("No safe migration is warranted."),
        ))))
    }
}

#[async_trait]
impl Provider for UnexpectedToolCallProvider {
    fn name(&self) -> &str {
        "unexpected-tool-call-plan-review"
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
        let call_id = format!("unexpected-tool-call-{index}");
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
struct RecordingPlanReviewControlBatches {
    batches: Vec<Vec<ControlEntry>>,
}

impl EventHandler for RecordingPlanReviewControlBatches {
    fn handle(&mut self, _event: RunEvent) -> Result<()> {
        Ok(())
    }

    fn commit_controls(
        &mut self,
        session: &mut Session,
        controls: Vec<ControlEntry>,
    ) -> Result<Vec<sigil_kernel::StoredEvent>> {
        self.batches.push(controls.clone());
        let events = session.append_controls_with_events(controls)?;
        Ok(events)
    }
}

/// Models an application bridge bound to the parent session. A plan-review child must never
/// delegate its control append to this handler because it owns a different durable store.
#[derive(Default)]
struct RejectForwardedPlanReviewChildControls {
    parent_scope_id: Option<String>,
    parent_store_path: Option<Option<std::path::PathBuf>>,
    events: Vec<RunEvent>,
}

impl EventHandler for RejectForwardedPlanReviewChildControls {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        self.events.push(event);
        Ok(())
    }

    fn commit_controls(
        &mut self,
        session: &mut Session,
        controls: Vec<ControlEntry>,
    ) -> Result<Vec<sigil_kernel::StoredEvent>> {
        let scope_id = session.session_scope_id().to_owned();
        let store_path = session.store_path().map(std::path::Path::to_path_buf);
        match (&self.parent_scope_id, &self.parent_store_path) {
            (Some(expected_scope), Some(expected_store)) => {
                if expected_scope != &scope_id || expected_store != &store_path {
                    bail!("plan-review child controls must not be forwarded to the parent handler");
                }
            }
            (None, None) => {
                self.parent_scope_id = Some(scope_id);
                self.parent_store_path = Some(store_path);
            }
            _ => bail!("plan-review parent handler binding is incomplete"),
        }
        let events = session.append_controls_with_events(controls.clone())?;
        for control in controls {
            self.handle(RunEvent::Control(control))?;
        }
        Ok(events)
    }
}

#[derive(Default)]
struct AskingPlanReviewProvider {
    calls: AtomicUsize,
    request_tools: Arc<Mutex<Vec<Vec<String>>>>,
    request_messages: Arc<Mutex<Vec<Vec<ModelMessage>>>>,
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
        self.request_messages
            .lock()
            .map_err(|_| anyhow!("request messages lock poisoned"))?
            .push(request.messages.clone());
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if call == 0 {
            let args = r#"{
                "questions": [{
                    "id": "scope",
                    "question": "Which module should be migrated first?"
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

fn compact_plan_input_fixture(session: &Session, tag: &str) -> Result<()> {
    use sigil_kernel::{
        AdaptiveTailPolicyV3, COMPACTION_TOKEN_PROOF_SCHEMA_VERSION, CompactionFoldPlan,
        CompactionInitiation, ContinuationModelOutputV1, EffectiveTokenBudget,
        FrozenProviderRequestMaterial, InputTokenEvidence, PortableSemanticCompactionRequest,
        PortableTargetRequestMaterial, RequestFitProof, TokenMeasurementBinding,
        TokenMeasurementScope, ToolOutputProjectionPolicy, VersionedProfileIdentity,
    };
    let store = JsonlSessionStore::new(session.store_path().context("durable fixture")?)?;
    let records = session.read_durable_event_records()?;
    let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &records,
        AdaptiveTailPolicyV3 {
            tail_target_min_tokens: 1,
            tail_target_max_tokens: 1,
            ..AdaptiveTailPolicyV3::default()
        },
        90_000,
        None,
    )?;
    assert!(
        !plan.folded_event_ids.is_empty(),
        "fixture must really fold raw history"
    );
    let preflight =
        store.prepare_portable_semantic_compaction(PortableSemanticCompactionRequest {
            attempt_id: format!("{tag}-attempt"),
            compaction_id: format!("{tag}-compaction"),
            initiation: CompactionInitiation::Manual,
            base_projection_revision: format!("{tag}-revision"),
            branch_id: None,
            valid_for_snapshot: Some(format!("{tag}-snapshot")),
            objective: None,
            language: "en".to_owned(),
            plan,
            model_output: ContinuationModelOutputV1 {
                in_progress: Vec::new(),
                pending_actions: Vec::new(),
                provider_continuity: Vec::new(),
                model_notes: Vec::new(),
            },
            tool_output_projection_policy: ToolOutputProjectionPolicy::default(),
            started_at_unix_ms: 200,
            completed_at_unix_ms: 201,
        })?;
    let frozen = FrozenProviderRequestMaterial::freeze(
        session.session_scope_id(),
        CompletionRequest {
            provider_name: session.provider_name().to_owned(),
            model_name: session.model_name().to_owned(),
            messages: preflight.candidate_messages().to_vec(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(20),
            reasoning_effort: None,
            previous_response_handle: None,
            continuation_states: Vec::new(),
            traffic_partition_key: None,
            background: false,
            store: false,
            deterministic_materialization: true,
            hosted_tools: Vec::new(),
        },
    )?;
    let profile = |name: &str| VersionedProfileIdentity::from_content(name, 1, name.as_bytes());
    let binding = TokenMeasurementBinding {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        provider_name: session.provider_name().to_owned(),
        model_name: session.model_name().to_owned(),
        wire_profile: profile("plan-input-wire"),
        token_measurement_profile: profile("plan-input-tokenizer"),
        hosted_parity_profile: Some(profile("plan-input-hosted")),
    };
    let evidence = |tokens| InputTokenEvidence::Exact {
        tokens,
        material_fingerprint: frozen.fingerprint().to_owned(),
        measurement_scope: TokenMeasurementScope::RenderedTargetInput,
        binding: binding.clone(),
        provider_model_snapshot: None,
        provider_system_fingerprint: None,
    };
    let before = evidence(80);
    let proof = RequestFitProof {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        input: evidence(10),
        budget: EffectiveTokenBudget {
            schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            budget_profile: profile("plan-input-budget"),
            context_window_tokens: 100,
            requested_output_tokens: 20,
            safety_buffer_tokens: 10,
        },
    };
    let target = PortableTargetRequestMaterial::new(frozen.clone(), binding, proof)
        .with_portable_economics(&frozen, before)?;
    store.execute_portable_semantic_compaction(preflight, target)?;
    Ok(())
}

fn seed_completed_plan_input_history(session: &mut Session, tag: &str) -> Result<()> {
    for ordinal in 0..5 {
        session.append_user_message(ModelMessage::user(format!("{tag} user {ordinal}")))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some(format!("{tag} assistant {ordinal}")),
            Vec::new(),
        ))?;
    }
    Ok(())
}

#[tokio::test]
async fn plan_review_research_question_resumes_the_same_attempt_from_its_child_session()
-> Result<()> {
    plan_review_waiting_input_case(false, false).await
}

#[tokio::test]
async fn plan_review_explicit_waiting_keeps_first_started_prefix_after_parent_change() -> Result<()>
{
    plan_review_waiting_input_case(true, false).await
}

#[tokio::test]
async fn plan_review_waiting_preserves_answer_after_child_compaction_and_restart() -> Result<()> {
    plan_review_waiting_input_case(false, true).await
}

async fn plan_review_waiting_input_case(explicit: bool, compact_child: bool) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let parent_path = temp.path().join("sessions/session.jsonl");
    let (mut parent_session, request) = seed_route_decision_with_prior_context(
        Session::new("plan-review-test", "planned-model")
            .with_store(JsonlSessionStore::new(&parent_path)?),
        true,
    )?;
    let request = if explicit {
        PlanReviewCoordinator::prepare_explicit_plan_review(
            &mut parent_session,
            "build the fixed explicit plan",
            "explicit-review-run",
            None,
            90,
        )?
    } else {
        request
    };
    if compact_child {
        let mut child =
            Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
                request
                    .child_session_ref
                    .resolve(temp.path().join("sessions").as_path()),
            )?);
        child.ensure_identity_entry()?;
        seed_completed_plan_input_history(&mut child, "retained research evidence")?;
    }
    ensure_test_plan_review_attempt_started(&mut parent_session, &request, 100)?;
    let provider = AskingPlanReviewProvider::default();
    let request_tools = Arc::clone(&provider.request_tools);
    let request_messages = Arc::clone(&provider.request_messages);
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
    close_test_plan_review_run(
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

    if compact_child {
        let child = Session::load_from_store(
            "plan-review-test",
            "planned-model",
            JsonlSessionStore::new(
                request
                    .child_session_ref
                    .resolve(temp.path().join("sessions").as_path()),
            )?,
        )?;
        compact_plan_input_fixture(&child, "waiting-child")?;
    }

    parent_session
        .append_user_message(ModelMessage::user("unrelated parent change while waiting"))?;
    drop(parent_session);
    let mut parent_session = Session::load_from_store(
        "plan-review-test",
        "planned-model",
        JsonlSessionStore::new(&parent_path)?,
    )?;

    let (receipt, resumed, terminal_outbox) =
        PlanReviewCoordinator::accept_plan_review_research_input(
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
    assert!(terminal_outbox.is_none());
    assert!(receipt.continuation_required);
    let resumed = resumed.context("submitted research answer must resume the attempt")?;
    assert_eq!(resumed.attempt_id, request.attempt_id);
    ensure_test_plan_review_attempt_started(&mut parent_session, &resumed, 130)?;
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
    commit_test_plan_review_draft(&mut parent_session, &draft, &resumed, 140)?;
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
    let messages = request_messages
        .lock()
        .map_err(|_| anyhow!("request messages lock poisoned"))?;
    assert_eq!(messages.len(), 2);
    for request_messages in messages.iter() {
        let objective = request_messages
            .iter()
            .position(|message| message.content.as_deref() == Some(request.objective.as_str()))
            .context("objective missing")?;
        let prior_user = request_messages
            .iter()
            .position(|message| message.id == "user-prior")
            .context("original prior user")?;
        let prior_assistant = request_messages
            .iter()
            .position(|message| message.id == "assistant-prior")
            .context("original prior assistant")?;
        assert!(prior_user < prior_assistant && prior_assistant < objective);
        assert_eq!(
            request_messages
                .iter()
                .filter(|message| message.content.as_deref() == Some(request.objective.as_str()))
                .count(),
            1
        );
        assert!(
            !request_messages
                .iter()
                .any(|message| message.content.as_deref()
                    == Some("unrelated parent change while waiting"))
        );
    }
    let resumed_messages = &messages[1];
    let objective = resumed_messages
        .iter()
        .position(|message| message.content.as_deref() == Some(request.objective.as_str()))
        .expect("objective");
    let question = resumed_messages
        .iter()
        .position(|message| message.tool_calls.iter().any(|call| call.id == "ask-scope"))
        .context("original question missing")?;
    let answer = resumed_messages
        .iter()
        .position(|message| {
            message
                .content
                .as_deref()
                .is_some_and(|text| text.contains("crates/sigil-kernel"))
        })
        .context("accepted answer missing")?;
    assert!(
        objective < question && question < answer,
        "fixed source precedes original child question and accepted answer"
    );
    if compact_child {
        assert!(
            resumed_messages.iter().any(|message| message
                .content
                .as_deref()
                .is_some_and(|text| text.contains("retained research evidence"))),
            "compacted child evidence survives"
        );
        assert!(
            !resumed_messages
                .iter()
                .any(|message| message.content.as_deref()
                    == Some("retained research evidence assistant 0")),
            "folded raw child messages do not reappear"
        );
    }
    Ok(())
}

async fn managed_plan_review_research_waiting_fixture() -> Result<(
    tempfile::TempDir,
    Session,
    PlanReviewRunRequest,
    sigil_kernel::PublicUserInputRequestV1,
    Arc<dyn crate::plan_review_coordinator::PlanReviewChildResourceProvisionerV1>,
)> {
    let mut handler = NoopEventHandler;
    managed_plan_review_research_waiting_fixture_with_handler(&mut handler).await
}

async fn managed_plan_review_research_waiting_fixture_with_handler<H>(
    handler: &mut H,
) -> Result<(
    tempfile::TempDir,
    Session,
    PlanReviewRunRequest,
    sigil_kernel::PublicUserInputRequestV1,
    Arc<dyn crate::plan_review_coordinator::PlanReviewChildResourceProvisionerV1>,
)>
where
    H: EventHandler + Send,
{
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;

    let temp = tempfile::tempdir()?;
    let authority_state = temp.path().join("authority-state");
    let execution_temp = temp.path().join("execution-temp");
    std::fs::create_dir_all(authority_state.join("cache"))?;
    std::fs::create_dir_all(&execution_temp)?;
    let composition = crate::r71_authority_composition::compose_runtime_authority(
        &authority_state,
        &execution_temp,
        sigil_kernel::resource::CanonicalHash::from_bytes([0x8a; 32]),
        sigil_kernel::resource::AuthorityGeneration {
            epoch: 1,
            instance_hash: sigil_kernel::resource::CanonicalHash::from_bytes([0x75; 32]),
        },
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        )),
        &[
            Channel::SessionLog,
            Channel::ArtifactStaging,
            Channel::ArtifactStore,
        ],
    )?;
    let provisioner = composition.plan_review_child_resource_provisioner();
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let parent = Session::load_from_store("plan-review-test", "planned-model", parent_store)?;
    let (mut parent, request) = seed_route_decision(parent)?;
    let provider = AskingPlanReviewProvider::default();
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut approvals = AutoApproveHandler;
    let PlanReviewRunOutcome::AwaitingUserInput { request: pending } =
        PlanReviewCoordinator::run_plan_review_with_resource_provisioner(
            &mut parent,
            &request,
            &agent,
            plan_review_test_options(temp.path()),
            ToolRegistry::new(),
            handler,
            &mut approvals,
            RunCancellationOwner::new().handle(),
            Arc::clone(&provisioner),
        )
        .await?
    else {
        panic!("managed plan-review research must suspend for a durable question");
    };
    PlanReviewCoordinator::close_plan_review_run(
        &mut parent,
        &request,
        &PlanReviewRunOutcome::AwaitingUserInput {
            request: pending.clone(),
        },
        handler,
        110,
    )?;
    Ok((temp, parent, request, *pending, provisioner))
}

#[tokio::test]
async fn managed_plan_review_research_controls_stay_in_the_child_session() -> Result<()> {
    let mut handler = RejectForwardedPlanReviewChildControls::default();
    let (_temp, parent, request, pending, provisioner) =
        managed_plan_review_research_waiting_fixture_with_handler(&mut handler).await?;

    assert_eq!(
        handler.parent_scope_id.as_deref(),
        Some(parent.session_scope_id()),
        "the parent handler must bind the durable parent scope before child execution"
    );
    assert_eq!(
        handler.parent_store_path.as_ref(),
        Some(&parent.store_path().map(std::path::Path::to_path_buf)),
        "the parent handler must bind the durable parent writer before child execution"
    );

    assert!(handler.events.iter().filter(|event| matches!(event, RunEvent::Control(_))).all(|event| matches!(
        event,
        RunEvent::Control(ControlEntry::PlanReviewAttempt(attempt))
            if attempt.plan_review_id == request.plan_review_id
                && matches!(
                    attempt.status,
                    PlanReviewAttemptStatus::Started | PlanReviewAttemptStatus::WaitingForInput
                )
    )));
    assert!(
        !parent.entries().iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::UserInputRequested(requested))
                if requested.request.identity == pending.identity
        )),
        "the parent must only mirror the waiting attempt, not own the child request"
    );

    let bundle = provisioner.provision(&request)?;
    let child = Session::load_from_store(
        parent.provider_name(),
        parent.model_name(),
        JsonlSessionStore::new(bundle.session_log_path())?,
    )?;
    assert!(
        child.entries().iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::UserInputRequested(requested))
                if requested.request.identity == pending.identity
        )),
        "the exact request must remain durably owned by the managed research child"
    );
    bundle.finish()?;
    Ok(())
}

#[tokio::test]
async fn managed_plan_review_recovery_reopens_the_exact_child_submitted_receipt() -> Result<()> {
    let (_temp, parent, request, pending, provisioner) =
        managed_plan_review_research_waiting_fixture().await?;
    assert!(
        !PlanReviewCoordinator::managed_plan_review_research_input_allows_resume(
            &parent,
            &request,
            provisioner.as_ref(),
        )?,
        "a still-requested child question must leave the parent Waiting"
    );
    let mut stale_request = request.clone();
    stale_request.attempt_ordinal = stale_request.attempt_ordinal.saturating_add(1);
    let stale = PlanReviewCoordinator::managed_plan_review_research_input_allows_resume(
        &parent,
        &stale_request,
        provisioner.as_ref(),
    )
    .expect_err("a stale revision request must not claim a child receipt");
    assert!(
        stale
            .to_string()
            .contains("does not match the requested revision lineage"),
        "stale request must fail before a child resume admission: {stale:#}"
    );
    let bundle = provisioner.provision(&request)?;
    let mut child = Session::load_from_store(
        parent.provider_name(),
        parent.model_name(),
        JsonlSessionStore::new(bundle.session_log_path())?,
    )?;
    let command = sigil_kernel::UserInputDecisionCommandV1 {
        identity: pending.identity.clone(),
        request_hash: pending.request_hash.clone(),
        command_id: sigil_kernel::UserInputCommandId::new("managed-research-recovery-answer")?,
        decision: sigil_kernel::UserInputDecisionV1::Submitted {
            answers: vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "crates/sigil-kernel".to_owned(),
                },
            }],
        },
    };
    sigil_kernel::accept_user_input_decision(&mut child, command.clone(), 120)?;
    bundle.finish()?;

    let recovered = PlanReviewCoordinator::recover_managed_plan_review_research_input(
        &parent,
        provisioner.as_ref(),
    )?
    .expect("accepted managed research answer must recover while parent remains Waiting");
    assert_eq!(recovered, command);
    assert!(
        PlanReviewCoordinator::managed_plan_review_research_input_allows_resume(
            &parent,
            &request,
            provisioner.as_ref(),
        )?,
        "the exact submitted child receipt must admit the worker-side resume"
    );
    assert_eq!(
        PlanReviewProjection::from_entries(parent.entries())
            .latest_attempt(&request.plan_review_id)
            .map(|attempt| attempt.status),
        Some(PlanReviewAttemptStatus::WaitingForInput),
        "recovery reads the child receipt; it never fabricates a parent terminal"
    );
    Ok(())
}

#[tokio::test]
async fn managed_plan_review_recovery_replays_resolved_child_cancel_for_waiting_parent()
-> Result<()> {
    let (_temp, parent, request, pending, provisioner) =
        managed_plan_review_research_waiting_fixture().await?;
    let bundle = provisioner.provision(&request)?;
    let mut child = Session::load_from_store(
        parent.provider_name(),
        parent.model_name(),
        JsonlSessionStore::new(bundle.session_log_path())?,
    )?;
    let command = sigil_kernel::UserInputDecisionCommandV1 {
        identity: pending.identity,
        request_hash: pending.request_hash,
        command_id: sigil_kernel::UserInputCommandId::new("managed-research-recovery-cancel")?,
        decision: sigil_kernel::UserInputDecisionV1::RunCancelled,
    };
    let receipt = sigil_kernel::accept_user_input_decision(&mut child, command.clone(), 120)?;
    assert!(matches!(
        receipt.request.resolution,
        Some(sigil_kernel::UserInputResolutionV1::RunCancelled)
    ));
    bundle.finish()?;

    let recovered = PlanReviewCoordinator::recover_managed_plan_review_research_input(
        &parent,
        provisioner.as_ref(),
    )?
    .expect("resolved managed child cancel must recover while parent remains Waiting");
    assert_eq!(recovered, command);
    assert!(
        !PlanReviewCoordinator::managed_plan_review_research_input_allows_resume(
            &parent,
            &request,
            provisioner.as_ref(),
        )?,
        "child cancellation must remain on the parent terminal-settlement path"
    );
    Ok(())
}

#[tokio::test]
async fn managed_plan_review_declined_research_input_allows_the_same_attempt_to_resume()
-> Result<()> {
    let (_temp, parent, request, pending, provisioner) =
        managed_plan_review_research_waiting_fixture().await?;
    let bundle = provisioner.provision(&request)?;
    let mut child = Session::load_from_store(
        parent.provider_name(),
        parent.model_name(),
        JsonlSessionStore::new(bundle.session_log_path())?,
    )?;
    let command = sigil_kernel::UserInputDecisionCommandV1 {
        identity: pending.identity,
        request_hash: pending.request_hash,
        command_id: sigil_kernel::UserInputCommandId::new("managed-research-recovery-decline")?,
        decision: sigil_kernel::UserInputDecisionV1::Declined,
    };
    let receipt = sigil_kernel::accept_user_input_decision(&mut child, command, 120)?;
    assert!(matches!(
        receipt.request.resolution,
        Some(sigil_kernel::UserInputResolutionV1::Declined)
    ));
    bundle.finish()?;

    assert!(
        PlanReviewCoordinator::managed_plan_review_research_input_allows_resume(
            &parent,
            &request,
            provisioner.as_ref(),
        )?,
        "a declined clarification can settle the same attempt without another research dispatch"
    );
    assert_eq!(
        PlanReviewProjection::from_entries(parent.entries())
            .latest_attempt(&request.plan_review_id)
            .map(|attempt| attempt.status),
        Some(PlanReviewAttemptStatus::WaitingForInput),
        "the parent remains recoverable until the worker owns Started"
    );
    Ok(())
}

#[tokio::test]
async fn managed_plan_review_recovery_fails_closed_without_recreating_a_missing_child_record()
-> Result<()> {
    let (_temp, parent, request, _pending, provisioner) =
        managed_plan_review_research_waiting_fixture().await?;
    let bundle = provisioner.provision(&request)?;
    let child_path = bundle.session_log_path().to_path_buf();
    bundle.finish()?;
    std::fs::remove_file(&child_path)?;

    let error = PlanReviewCoordinator::recover_managed_plan_review_research_input(
        &parent,
        provisioner.as_ref(),
    )
    .expect_err("missing managed child record must not be reinitialized during recovery");
    assert!(
        error.to_string().contains("recovery"),
        "existing-only admission must report the missing child instead of opening a fallback: {error:#}"
    );
    assert!(
        !child_path.exists(),
        "recovery must not recreate the missing child record or an unmanaged fallback"
    );
    assert_eq!(
        PlanReviewProjection::from_entries(parent.entries())
            .latest_attempt(&request.plan_review_id)
            .map(|attempt| attempt.status),
        Some(PlanReviewAttemptStatus::WaitingForInput),
        "a recovery admission failure retains the durable parent attention fact"
    );
    Ok(())
}

#[tokio::test]
async fn managed_plan_review_input_acceptance_fails_closed_without_recreating_a_missing_child_record()
-> Result<()> {
    let (_temp, mut parent, request, pending, provisioner) =
        managed_plan_review_research_waiting_fixture().await?;
    let bundle = provisioner.provision(&request)?;
    let child_path = bundle.session_log_path().to_path_buf();
    bundle.finish()?;
    std::fs::remove_file(&child_path)?;
    let command = sigil_kernel::UserInputDecisionCommandV1 {
        identity: pending.identity,
        request_hash: pending.request_hash,
        command_id: sigil_kernel::UserInputCommandId::new("managed-research-missing-child")?,
        decision: sigil_kernel::UserInputDecisionV1::RunCancelled,
    };

    let error = PlanReviewCoordinator::accept_plan_review_research_input_with_resources(
        &mut parent,
        command,
        120,
        provisioner.as_ref(),
    )
    .expect_err("accepted input must not recreate a missing managed child record");
    assert!(
        error.to_string().contains("mutation admission"),
        "existing-only input admission must reject the missing child: {error:#}"
    );
    assert!(
        !child_path.exists(),
        "input acceptance must not recreate a missing child record, namespace, or fallback"
    );
    assert_eq!(
        PlanReviewProjection::from_entries(parent.entries())
            .latest_attempt(&request.plan_review_id)
            .map(|attempt| attempt.status),
        Some(PlanReviewAttemptStatus::WaitingForInput),
        "a child admission failure retains the durable parent attention fact"
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_research_uses_the_ordinary_model_loop_without_turn_checkpoint() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent_session, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/session.jsonl"),
        )?),
    )?;
    let provider = LoopingPlanReviewProvider::default();
    let request_tools = Arc::clone(&provider.request_tools);
    let request_messages = Arc::clone(&provider.request_messages);
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

    assert!(
        matches!(outcome, PlanReviewRunOutcome::CompletedWithoutDraft),
        "unexpected outcome: {outcome:?}"
    );
    assert!(
        owner.handle().is_naturally_finalized(),
        "the coordinator must claim the root terminal only after its internal phases complete"
    );
    let requests = request_tools
        .lock()
        .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
    assert_eq!(
        requests.len(),
        9,
        "research uses the ordinary model loop and caller budget"
    );
    assert!(requests.iter().all(|tools| {
        tools.iter().any(|name| name == "inspect_workspace")
            && tools
                .iter()
                .any(|name| name == sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME)
    }));
    let messages = request_messages
        .lock()
        .map_err(|_| anyhow!("plan review message recorder lock poisoned"))?;
    assert!(
        messages.iter().flatten().all(|message| {
            message.content.as_deref().is_none_or(|content| {
                !content.contains("Reassess the evidence gathered so far")
                    && !content.contains("eighth research turn")
            })
        }),
        "the host adds no turn-count checkpoint to the model's ordinary loop"
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_typed_no_plan_closes_in_the_ordinary_loop() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent_session, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/session.jsonl"),
        )?),
    )?;
    let provider = NoPlanPlanReviewProvider::default();
    let request_tools = Arc::clone(&provider.request_tools);
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut handler = NoopEventHandler;
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
    assert!(matches!(
        outcome,
        PlanReviewRunOutcome::CompletedWithoutDraft
    ));

    let requests = request_tools
        .lock()
        .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
    assert_eq!(
        requests.len(),
        1,
        "no_plan must close the current review loop"
    );
    let child_path = request
        .child_session_ref
        .resolve(temp.path().join("sessions").as_path());
    let child = Session::load_from_store(
        parent_session.provider_name(),
        parent_session.model_name(),
        JsonlSessionStore::new(child_path)?,
    )?;
    assert!(!child.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanDraftCreated(_))
    )));
    assert!(child.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::ToolResultV3(result)
                if result.tool_name == sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME
                    && result.initial_model_view.preview.contains("no_plan")
        )
    }));
    Ok(())
}

#[tokio::test]
async fn plan_review_research_receives_frozen_parent_discussion_before_source_objective()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent_session, request) = seed_route_decision_with_prior_context(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/session.jsonl"),
        )?),
        true,
    )?;
    let provider = LoopingPlanReviewProvider::default();
    let request_messages = Arc::clone(&provider.request_messages);
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
    assert!(matches!(
        outcome,
        PlanReviewRunOutcome::CompletedWithoutDraft
    ));

    let requests = request_messages
        .lock()
        .map_err(|_| anyhow!("plan review message recorder lock poisoned"))?;
    let first = requests
        .first()
        .context("research request was not recorded")?;
    let content = first
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>();
    let prior_index = content
        .iter()
        .position(|text| text.contains("preserve this earlier migration constraint"))
        .context("frozen parent user context was not sent to research")?;
    let prior_assistant_index = content
        .iter()
        .position(|text| text.contains("migration must remain reversible"))
        .context("frozen parent assistant context was not sent to research")?;
    let objective_index = content
        .iter()
        .position(|text| **text == request.objective)
        .context("source objective was not sent to research")?;
    assert!(prior_index < prior_assistant_index);
    assert!(prior_assistant_index < objective_index);
    assert_eq!(
        content
            .iter()
            .filter(|text| **text == request.objective)
            .count(),
        1
    );
    assert!(requests.len() >= 4, "must exercise multiple research turns");
    for messages in requests.iter() {
        let objective_index = messages
            .iter()
            .position(|message| message.content.as_deref() == Some(request.objective.as_str()))
            .context("request lost its objective")?;
        assert_eq!(
            messages
                .iter()
                .filter(|message| {
                    message.content.as_deref() == Some(request.objective.as_str())
                })
                .count(),
            1
        );
        assert_eq!(messages[objective_index - 2].id, "user-prior");
        assert_eq!(messages[objective_index - 1].id, "assistant-prior");
        assert!(
            messages[..objective_index].iter().all(|message| {
                message.tool_calls.is_empty() && message.role != sigil_kernel::MessageRole::Tool
            }),
            "child tool history must follow the original objective"
        );
    }
    let child_path = request
        .child_session_ref
        .resolve(temp.path().join("sessions").as_path());
    let child = Session::load_from_store(
        parent_session.provider_name(),
        parent_session.model_name(),
        JsonlSessionStore::new(child_path)?,
    )?;
    assert!(child.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(candidate))
                if candidate.plan_review_id == request.plan_review_id
                    && candidate.attempt_id == request.attempt_id
                    && candidate.plan_id == request.plan_id
                    // An untyped research final answer is durable evidence, but it does not
                    // prove that the prose is a complete Plan. It must remain non-adoptable
                    // until a typed result or explicit user review supplies that proof.
                    && candidate.completeness
                        == sigil_kernel::PlanReviewCandidateCompletenessV1::Unknown
                    && candidate.content == "research did not converge"
        )
    }));
    Ok(())
}

#[tokio::test]
async fn plan_review_allows_effect_proven_web_and_trusted_mcp_reads_but_denies_writes() -> Result<()>
{
    let cases = [
        (
            PlanReviewEffectProbeKind::WebRead,
            "websearch",
            NetworkPolicy::Allow,
            true,
        ),
        (
            PlanReviewEffectProbeKind::TrustedMcpRead,
            "mcp__trusted__search",
            NetworkPolicy::Allow,
            true,
        ),
        (
            PlanReviewEffectProbeKind::WebRead,
            "websearch",
            NetworkPolicy::Deny,
            false,
        ),
        (
            PlanReviewEffectProbeKind::WorkspaceWrite,
            "write_file",
            NetworkPolicy::Allow,
            false,
        ),
    ];

    for (kind, tool_name, network_policy, should_execute) in cases {
        let temp = tempfile::tempdir()?;
        let (mut parent_session, request) = seed_route_decision(
            Session::new("plan-review-effect-probe", "planned-model")
                .with_store(JsonlSessionStore::new(temp.path().join("session.jsonl"))?),
        )?;
        let executions = Arc::new(AtomicUsize::new(0));
        let provider = PlanReviewEffectProbeProvider {
            tool_name: tool_name.to_owned(),
            requested_tool_surfaces: Arc::new(Mutex::new(Vec::new())),
            observed_permission_error: Arc::new(AtomicBool::new(false)),
        };
        let surfaces = Arc::clone(&provider.requested_tool_surfaces);
        let permission_error = Arc::clone(&provider.observed_permission_error);
        let agent = Agent::new(provider, ToolRegistry::new());
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(PlanReviewEffectProbeTool {
            kind,
            executions: Arc::clone(&executions),
        }));
        let mut options = plan_review_test_options(temp.path());
        options.permission_context.network_policy = network_policy;

        let mut handler = NoopEventHandler;
        let mut approval_handler = AutoApproveHandler;
        let outcome = PlanReviewCoordinator::run_plan_review(
            &mut parent_session,
            &request,
            &agent,
            options,
            registry,
            &mut handler,
            &mut approval_handler,
            RunCancellationOwner::new().handle(),
        )
        .await?;

        assert!(
            matches!(outcome, PlanReviewRunOutcome::CompletedWithoutDraft),
            "the model should receive the effect result and finish normally for {tool_name}"
        );
        let surfaces = surfaces
            .lock()
            .map_err(|_| anyhow!("PlanReview effect probe surface lock poisoned"))?;
        assert!(
            surfaces[0].iter().any(|name| name == tool_name),
            "PlanReview should expose {tool_name} to per-call effect analysis"
        );
        assert_eq!(
            executions.load(Ordering::SeqCst) > 0,
            should_execute,
            "unexpected execution outcome for {tool_name} with network policy {network_policy:?}"
        );
        if matches!(kind, PlanReviewEffectProbeKind::WorkspaceWrite) {
            assert!(
                permission_error.load(Ordering::SeqCst),
                "read-only permission failure must return to the ordinary model loop"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn plan_review_does_not_dispatch_unsolicited_tool_calls() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent_session, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/session.jsonl"),
        )?),
    )?;
    let provider = UnexpectedToolCallProvider::default();
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
        PlanReviewRunOutcome::CompletedWithoutDraft
    ));
    let requests = request_tools
        .lock()
        .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
    assert_eq!(requests.len(), 1, "no extra host-driven model turn");
    assert!(requests[0].iter().any(|tool| tool == "inspect_workspace"));
    Ok(())
}

#[tokio::test]
async fn plan_review_invalid_draft_can_be_corrected_in_the_ordinary_tool_loop() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent_session, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/session.jsonl"),
        )?),
    )?;
    let provider = InvalidThenCorrectedDraftProvider::default();
    let calls = Arc::clone(&provider.request_tools);
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(PlanReviewInspectionTool));
    let outcome = PlanReviewCoordinator::run_plan_review(
        &mut parent_session,
        &request,
        &agent,
        plan_review_test_options(temp.path()),
        registry,
        &mut NoopEventHandler,
        &mut AutoApproveHandler,
        RunCancellationOwner::new().handle(),
    )
    .await?;

    let PlanReviewRunOutcome::DraftReady { draft } = outcome else {
        panic!("the corrected typed submission must produce a ready draft");
    };
    let requests = calls
        .lock()
        .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
    assert_eq!(
        requests.len(),
        2,
        "the model corrects its rejected submission"
    );
    assert!(requests.iter().all(|tools| {
        tools.iter().any(|tool| tool == "inspect_workspace")
            && tools
                .iter()
                .any(|tool| tool == sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME)
    }));
    assert_eq!(requests[0], requests[1], "research tools remain available");
    commit_test_plan_review_draft(&mut parent_session, &draft, &request, 100)?;
    assert!(
        parent_session
            .plan_artifact_projection()
            .plans
            .get(&request.plan_id)
            .and_then(|plan| plan.inline_text.as_deref())
            .is_some_and(|content| content.contains("Bounded plan review"))
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_text_does_not_trigger_host_draft_validation_retries() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent_session, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/session.jsonl"),
        )?),
    )?;
    let provider = InvalidThenValidSubmissionProvider::default();
    let request_tools = Arc::clone(&provider.request_tools);
    let request_messages = Arc::clone(&provider.request_messages);
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

    assert!(matches!(
        outcome,
        PlanReviewRunOutcome::CompletedWithoutDraft
    ));
    let requests = request_tools
        .lock()
        .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
    assert_eq!(
        requests.len(),
        1,
        "text alone completes the ordinary loop without a host-driven draft retry"
    );
    assert!(handler.0.iter().all(|event| !matches!(
        event,
        RunEvent::Notice(message) if message.contains("invalid typed draft")
    )));
    assert_eq!(
        request_messages
            .lock()
            .map_err(|_| anyhow!("message lock poisoned"))?
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_plain_text_remains_an_unconfirmed_candidate() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent_session, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/session.jsonl"),
        )?),
    )?;
    let provider = PlainTextOnlyReviewProvider::default();
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

    assert!(matches!(
        outcome,
        PlanReviewRunOutcome::CompletedWithoutDraft
    ));
    PlanReviewCoordinator::complete_without_draft(&mut parent_session, &request, &mut handler, 42)?;
    let review = crate::conversation_display::public_plan_review_from_entries(
        parent_session.entries(),
        None,
    )?
    .expect("candidate review should remain visible");
    assert_eq!(
        review.status,
        sigil_kernel::PublicPlanReviewStatus::CompletedWithoutDraft
    );
    let candidate = review.candidate.expect("candidate should be projected");
    assert!(
        candidate
            .content
            .contains("workspace inspection is complete")
    );
    assert_eq!(
        candidate.completeness,
        sigil_kernel::PlanReviewCandidateCompletenessV1::Unknown
    );
    assert!(parent_session.plan_artifact_projection().plans.is_empty());
    assert!(
        sigil_kernel::TaskStateProjection::from_entries(parent_session.entries())
            .tasks
            .is_empty()
    );
    assert_eq!(
        review.allowed_actions,
        Vec::<sigil_kernel::PublicPlanAction>::new()
    );
    let requests = request_tools
        .lock()
        .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
    assert_eq!(
        requests.len(),
        1,
        "plain text is preserved without a typed submission"
    );
    Ok(())
}

#[test]
fn adopting_a_complete_candidate_creates_only_a_reviewable_draft() -> Result<()> {
    let (mut session, request) = session_with_route_decision()?;
    let mut handler = sigil_kernel::NoopEventHandler;
    PlanReviewCoordinator::ensure_attempt_started(&mut session, &request, &mut handler, 10)?;
    let candidate = sigil_kernel::plan_review_candidate_recorded_entry(
        request.plan_review_id.clone(),
        request.attempt_id.clone(),
        request.plan_id.clone(),
        request.plan_source_ref(),
        Some("assistant-candidate".to_owned()),
        "# Plan\n\n1. Preserve the exact candidate.",
        sigil_kernel::PlanReviewCandidateCompletenessV1::Complete,
        11,
    )?;
    session.append_control(ControlEntry::PlanReviewCandidateRecordedV1(Box::new(
        candidate.clone(),
    )))?;
    PlanReviewCoordinator::close_plan_review_run(
        &mut session,
        &request,
        &PlanReviewRunOutcome::Paused("candidate classification is unresolved".to_owned()),
        &mut handler,
        11,
    )?;
    let draft = PlanReviewCoordinator::adopt_plan_review_candidate(
        &mut session,
        &request.plan_id,
        &candidate.content_hash,
        &mut handler,
        12,
    )?;
    assert_eq!(draft.plan_hash, candidate.content_hash);
    assert!(
        session
            .plan_artifact_projection()
            .plans
            .contains_key(&request.plan_id)
    );
    assert_eq!(
        PlanReviewProjection::from_entries(session.entries())
            .latest_attempt(&request.plan_review_id)
            .map(|attempt| attempt.status),
        Some(PlanReviewAttemptStatus::DraftReady)
    );
    let attempts = PlanReviewProjection::from_entries(session.entries())
        .review(&request.plan_review_id)
        .expect("review projection")
        .attempts
        .clone();
    assert_eq!(
        attempts.len(),
        3,
        "adoption must append a successor attempt"
    );
    assert_eq!(attempts[1].status, PlanReviewAttemptStatus::Paused);
    assert_ne!(attempts[1].attempt_id, attempts[2].attempt_id);
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanReviewResolutionRecordedV1(resolution))
            if resolution.candidate_hash == candidate.content_hash
                && resolution.outcome == sigil_kernel::PlanReviewResultOutcome::Draft
                && resolution.actor == sigil_kernel::PlanReviewResolutionActorV1::User
    )));
    let replay = PlanReviewCoordinator::adopt_plan_review_candidate(
        &mut session,
        &request.plan_id,
        &candidate.content_hash,
        &mut handler,
        13,
    )?;
    assert_eq!(replay.plan_hash, draft.plan_hash);
    Ok(())
}

#[test]
fn unknown_candidate_completeness_never_exposes_adopt_action() -> Result<()> {
    let (mut session, request) = session_with_route_decision()?;
    let mut handler = sigil_kernel::NoopEventHandler;
    PlanReviewCoordinator::ensure_attempt_started(&mut session, &request, &mut handler, 10)?;
    let candidate = sigil_kernel::plan_review_candidate_recorded_entry(
        request.plan_review_id.clone(),
        request.attempt_id.clone(),
        request.plan_id.clone(),
        request.plan_source_ref(),
        Some("assistant-unknown".to_owned()),
        "research prose without a completion proof",
        sigil_kernel::PlanReviewCandidateCompletenessV1::Unknown,
        11,
    )?;
    session.append_control(ControlEntry::PlanReviewCandidateRecordedV1(Box::new(
        candidate,
    )))?;
    PlanReviewCoordinator::close_plan_review_run(
        &mut session,
        &request,
        &PlanReviewRunOutcome::Paused("candidate classification is unresolved".to_owned()),
        &mut handler,
        12,
    )?;
    let review =
        crate::conversation_display::public_plan_review_from_entries(session.entries(), None)?
            .context("unknown candidate review should remain visible")?;
    assert_eq!(
        review
            .candidate
            .as_ref()
            .map(|candidate| candidate.completeness),
        Some(sigil_kernel::PlanReviewCandidateCompletenessV1::Unknown)
    );
    assert_eq!(
        review.allowed_actions,
        vec![sigil_kernel::PublicPlanAction::RetryReview]
    );
    Ok(())
}

#[test]
fn ordinary_plan_review_retry_uses_terminal_frontier_cas_and_successor_attempt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut session, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/session.jsonl"),
        )?),
    )?;
    let mut handler = sigil_kernel::NoopEventHandler;
    PlanReviewCoordinator::ensure_attempt_started(&mut session, &request, &mut handler, 10)?;
    PlanReviewCoordinator::close_plan_review_run(
        &mut session,
        &request,
        &PlanReviewRunOutcome::Paused("budget exhausted".to_owned()),
        &mut handler,
        11,
    )?;
    let predecessor = PlanReviewProjection::from_entries(session.entries())
        .latest_attempt(&request.plan_review_id)
        .cloned()
        .expect("terminal predecessor");
    let command = crate::PlanReviewRetryCommand {
        command_id: "retry-cas-1".to_owned(),
        plan_review_id: request.plan_review_id.clone(),
        expected_attempt_id: predecessor.attempt_id.clone(),
        expected_status: predecessor.status,
        expected_durable_frontier: session.durable_frontier_sequence(),
        expected_context_digest: crate::plan_review_context_digest_for_attempt(
            &session,
            &predecessor,
        )?,
        candidate_hash: None,
        blocker_id: None,
        workspace_snapshot_id: predecessor.workspace_snapshot_id.clone(),
    };
    let receipt = PlanReviewCoordinator::retry_plan_review(&mut session, &command, 12)?;
    assert!(!receipt.idempotent_replay);
    assert_ne!(receipt.predecessor_attempt_id, receipt.successor_attempt_id);
    assert_eq!(receipt.request.attempt_id, receipt.successor_attempt_id);
    let attempts = PlanReviewProjection::from_entries(session.entries())
        .review(&request.plan_review_id)
        .expect("review projection")
        .attempts
        .clone();
    assert_eq!(
        attempts.len(),
        3,
        "retry appends one successor after Started and Paused"
    );
    assert_eq!(attempts[1].status, PlanReviewAttemptStatus::Paused);
    assert_eq!(attempts[2].status, PlanReviewAttemptStatus::Started);

    let replay = PlanReviewCoordinator::retry_plan_review(&mut session, &command, 13)?;
    assert!(replay.idempotent_replay);
    assert_eq!(replay.successor_attempt_id, receipt.successor_attempt_id);
    Ok(())
}

#[test]
fn ordinary_plan_review_retry_for_public_plan_derives_the_exact_binding() -> Result<()> {
    let (mut session, request) = session_with_route_decision()?;
    let mut handler = sigil_kernel::NoopEventHandler;
    PlanReviewCoordinator::ensure_attempt_started(&mut session, &request, &mut handler, 10)?;
    PlanReviewCoordinator::close_plan_review_run(
        &mut session,
        &request,
        &PlanReviewRunOutcome::Failed("provider disconnected".to_owned()),
        &mut handler,
        11,
    )?;
    let receipt = PlanReviewCoordinator::retry_plan_review_for_plan(
        &mut session,
        &request.plan_id,
        None,
        12,
    )?;
    assert!(!receipt.idempotent_replay);
    assert_eq!(receipt.request.plan_review_id, request.plan_review_id);
    assert_eq!(receipt.request.attempt_ordinal, 2);
    assert_eq!(
        PlanReviewProjection::from_entries(session.entries())
            .latest_attempt(&request.plan_review_id)
            .map(|attempt| attempt.status),
        Some(PlanReviewAttemptStatus::Started)
    );
    Ok(())
}

#[test]
fn ordinary_plan_review_retry_rejects_stale_context_and_frontier() -> Result<()> {
    let (mut session, request) = session_with_route_decision()?;
    let mut handler = sigil_kernel::NoopEventHandler;
    PlanReviewCoordinator::ensure_attempt_started(&mut session, &request, &mut handler, 10)?;
    PlanReviewCoordinator::close_plan_review_run(
        &mut session,
        &request,
        &PlanReviewRunOutcome::Interrupted("provider stopped".to_owned()),
        &mut handler,
        11,
    )?;
    let predecessor = PlanReviewProjection::from_entries(session.entries())
        .latest_attempt(&request.plan_review_id)
        .cloned()
        .expect("terminal predecessor");
    let base = crate::PlanReviewRetryCommand {
        command_id: "retry-cas-stale".to_owned(),
        plan_review_id: request.plan_review_id.clone(),
        expected_attempt_id: predecessor.attempt_id.clone(),
        expected_status: predecessor.status,
        expected_durable_frontier: session.durable_frontier_sequence(),
        expected_context_digest: crate::plan_review_context_digest_for_attempt(
            &session,
            &predecessor,
        )?,
        candidate_hash: None,
        blocker_id: None,
        workspace_snapshot_id: predecessor.workspace_snapshot_id.clone(),
    };
    let mut stale_context = base.clone();
    stale_context.expected_context_digest = "sha256:stale".to_owned();
    assert!(
        PlanReviewCoordinator::retry_plan_review(&mut session, &stale_context, 12)
            .is_err_and(|error| error.to_string().contains("context binding is stale"))
    );
    let mut stale_frontier = base;
    stale_frontier.expected_durable_frontier =
        stale_frontier.expected_durable_frontier.saturating_add(1);
    assert!(
        PlanReviewCoordinator::retry_plan_review(&mut session, &stale_frontier, 12)
            .is_err_and(|error| error.to_string().contains("frontier is stale"))
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_hard_budget_stops_the_ordinary_loop() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent_session, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/session.jsonl"),
        )?),
    )?;
    let provider = PlainTextOnlyReviewProvider::default();
    let request_tools = Arc::clone(&provider.request_tools);
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut handler = RecordingPlanReviewEvents::default();
    let mut approval_handler = AutoApproveHandler;
    let mut options = plan_review_test_options(temp.path());
    options.max_turns = Some(1);

    let outcome = PlanReviewCoordinator::run_plan_review(
        &mut parent_session,
        &request,
        &agent,
        options,
        ToolRegistry::new(),
        &mut handler,
        &mut approval_handler,
        RunCancellationOwner::new().handle(),
    )
    .await?;

    assert!(matches!(
        outcome,
        PlanReviewRunOutcome::CompletedWithoutDraft
    ));
    let requests = request_tools
        .lock()
        .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
    assert_eq!(
        requests.len(),
        1,
        "the hard budget caps every provider call in the ordinary model loop"
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_user_input_resume_keeps_the_remaining_turn_budget() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let parent_path = temp.path().join("sessions/parent.jsonl");
    let (mut parent_session, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model")
            .with_store(JsonlSessionStore::new(&parent_path)?),
    )?;
    ensure_test_plan_review_attempt_started(&mut parent_session, &request, 100)?;
    let provider = AskingPlanReviewProvider::default();
    let requests = Arc::clone(&provider.request_tools);
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut options = plan_review_test_options(temp.path());
    options.max_turns = Some(1);
    let suspended = PlanReviewCoordinator::run_plan_review(
        &mut parent_session,
        &request,
        &agent,
        options.clone(),
        ToolRegistry::new(),
        &mut NoopEventHandler,
        &mut AutoApproveHandler,
        RunCancellationOwner::new().handle(),
    )
    .await?;
    let PlanReviewRunOutcome::AwaitingUserInput { request: pending } = suspended else {
        panic!("the first model turn should suspend for its durable question")
    };
    close_test_plan_review_run(
        &mut parent_session,
        &request,
        &PlanReviewRunOutcome::AwaitingUserInput {
            request: pending.clone(),
        },
        110,
    )?;
    drop(parent_session);

    let mut reopened_parent = Session::load_from_store(
        "plan-review-test",
        "planned-model",
        JsonlSessionStore::new(&parent_path)?,
    )?;
    let (receipt, resumed, terminal_outbox) =
        PlanReviewCoordinator::accept_plan_review_research_input(
            &mut reopened_parent,
            sigil_kernel::UserInputDecisionCommandV1 {
                identity: pending.identity.clone(),
                request_hash: pending.request_hash.clone(),
                command_id: sigil_kernel::UserInputCommandId::new(
                    "resume-plan-review-turn-budget",
                )?,
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
    assert!(terminal_outbox.is_none());
    assert!(receipt.continuation_required);
    let resumed = resumed.context("accepted research answer must resume the same attempt")?;
    ensure_test_plan_review_attempt_started(&mut reopened_parent, &resumed, 130)?;

    let resumed_outcome = PlanReviewCoordinator::run_plan_review(
        &mut reopened_parent,
        &resumed,
        &agent,
        options,
        ToolRegistry::new(),
        &mut NoopEventHandler,
        &mut AutoApproveHandler,
        RunCancellationOwner::new().handle(),
    )
    .await?;
    assert!(matches!(
        resumed_outcome,
        PlanReviewRunOutcome::Interrupted(_)
    ));
    assert_eq!(
        requests
            .lock()
            .map_err(|_| anyhow!("Plan Review request lock poisoned"))?
            .len(),
        1,
        "reopening the same attempt must not restore its already spent model turn"
    );
    close_test_plan_review_run(&mut reopened_parent, &resumed, &resumed_outcome, 140)?;
    Ok(())
}

#[tokio::test]
async fn plan_review_model_submitted_draft_controls_stay_in_the_research_child_session()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let parent_path = temp.path().join("sessions/session.jsonl");
    let (mut parent_session, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model")
            .with_store(JsonlSessionStore::new(&parent_path)?),
    )?;
    let agent = Agent::new(ResearchThenDraftProvider::default(), ToolRegistry::new());
    let mut handler = RejectForwardedPlanReviewChildControls::default();
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
    assert_eq!(
        handler.parent_scope_id.as_deref(),
        Some(parent_session.session_scope_id())
    );
    assert_eq!(
        handler.parent_store_path.as_ref(),
        Some(
            &parent_session
                .store_path()
                .map(std::path::Path::to_path_buf)
        )
    );
    assert!(
        handler
            .events
            .iter()
            .filter(|event| matches!(event, RunEvent::Control(_)))
            .all(|event| matches!(
                event,
                RunEvent::Control(ControlEntry::PlanReviewAttempt(attempt))
                    if attempt.plan_review_id == request.plan_review_id
                        && matches!(
                            attempt.status,
                            PlanReviewAttemptStatus::Started
                        )
            ) || matches!(
                event,
                RunEvent::Control(ControlEntry::PlanReviewCandidateRecordedV1(candidate))
                    if candidate.plan_review_id == request.plan_review_id
                        && candidate.attempt_id == request.attempt_id
            ))
    );
    assert!(handler.events.iter().all(|event| !matches!(
        event,
        RunEvent::Control(ControlEntry::UserInputRequested(_) | ControlEntry::PlanDraftCreated(_))
    )));
    assert!(
        !parent_session.entries().iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft))
                if draft.plan_id == request.plan_id
        )),
        "the coordinator commits the draft only after reading the settled child state"
    );

    let child = Session::load_from_store(
        parent_session.provider_name(),
        parent_session.model_name(),
        JsonlSessionStore::new(
            request.child_session_ref.resolve(
                parent_path
                    .parent()
                    .expect("parent session fixture path must have a directory"),
            ),
        )?,
    )?;
    assert!(
        child.entries().iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft))
                if draft.plan_id == request.plan_id
        )),
        "the validated draft source must remain durably owned by the research child"
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_stream_interruption_does_not_replay_the_provider_request() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent_session, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/session.jsonl"),
        )?),
    )?;
    let provider = InterruptedPlanReviewProvider::default();
    let request_tools = Arc::clone(&provider.request_tools);
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(PlanReviewInspectionTool));
    let owner = RunCancellationOwner::new();
    let mut handler = RecordingPlanReviewEvents::default();
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
    .expect_err("interrupted research requires an explicit recovery decision");
    assert!(format!("{error:#}").contains("plan review provider run failed"));
    assert!(!owner.handle().is_naturally_finalized());
    let requests = request_tools
        .lock()
        .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
    assert_eq!(
        requests.len(),
        1,
        "a failed request does not start a separate model loop"
    );
    assert!(requests[0].iter().any(|name| name == "inspect_workspace"));
    assert!(parent_session.plan_artifact_projection().plans.is_empty());
    Ok(())
}

#[tokio::test]
async fn plan_review_transport_uncertainty_is_not_automatically_replayed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent_session, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/session.jsonl"),
        )?),
    )?;
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
        "transport uncertainty must not trigger an automatic provider request replay"
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

    close_test_plan_review_run(
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
    close_test_plan_review_run(
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

    // A recoverable review boundary stays distinct from a domain failure in the durable attempt.
    let (mut blocked_session, blocked_request) = session_with_route_decision()?;
    let blocked_action = sigil_kernel::StartPlanReviewAction {
        decision_id: blocked_request
            .route_decision_id
            .clone()
            .expect("decision id"),
        plan_review_id: blocked_request.plan_review_id.clone(),
        plan_id: blocked_request.plan_id.clone(),
        source_turn: blocked_request.source_turn.clone(),
    };
    PlanReviewCoordinator::prepare_automatic_plan_review(
        &mut blocked_session,
        &blocked_action,
        None,
        100,
    )?;
    close_test_plan_review_run(
        &mut blocked_session,
        &blocked_request,
        &PlanReviewRunOutcome::Blocked("workspace evidence requires attention".to_owned()),
        120,
    )?;
    let blocked_projection = PlanReviewProjection::from_entries(blocked_session.entries());
    let blocked_attempt = blocked_projection
        .latest_attempt(&blocked_request.plan_review_id)
        .expect("attempt");
    assert_eq!(blocked_attempt.status, PlanReviewAttemptStatus::Blocked);
    assert_eq!(
        blocked_attempt.terminal_reason,
        Some(sigil_kernel::PlanReviewTerminalReason::RunBlocked)
    );

    let (mut paused_session, paused_request) = session_with_route_decision()?;
    let paused_action = sigil_kernel::StartPlanReviewAction {
        decision_id: paused_request
            .route_decision_id
            .clone()
            .expect("decision id"),
        plan_review_id: paused_request.plan_review_id.clone(),
        plan_id: paused_request.plan_id.clone(),
        source_turn: paused_request.source_turn.clone(),
    };
    PlanReviewCoordinator::prepare_automatic_plan_review(
        &mut paused_session,
        &paused_action,
        None,
        100,
    )?;
    close_test_plan_review_run(
        &mut paused_session,
        &paused_request,
        &PlanReviewRunOutcome::Paused("provider retry budget is exhausted".to_owned()),
        120,
    )?;
    let paused_projection = PlanReviewProjection::from_entries(paused_session.entries());
    let paused_attempt = paused_projection
        .latest_attempt(&paused_request.plan_review_id)
        .expect("attempt");
    assert_eq!(paused_attempt.status, PlanReviewAttemptStatus::Paused);
    assert_eq!(
        paused_attempt.terminal_reason,
        Some(sigil_kernel::PlanReviewTerminalReason::RunPaused)
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
    close_test_plan_review_run(
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
    close_test_plan_review_run_if_open(
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
    close_test_plan_review_run_if_open(
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

#[test]
fn plan_control_ablation_local_decisions_need_only_the_durable_session() -> Result<()> {
    for action in [
        crate::ApplicationPlanAction::Run,
        crate::ApplicationPlanAction::Save,
        crate::ApplicationPlanAction::Revise,
        crate::ApplicationPlanAction::Reject,
    ] {
        let (_temp, session, _request, draft) = durable_session_with_ready_plan()?;
        // This durable provider/model has no configured connection. Local decisions must not
        // require provider construction, credential lookup or a current composition match.
        assert_eq!(session.provider_name(), "plan-review-test");
        let session_path = session.store_path().context("durable fixture")?.to_owned();
        let scope = session.session_scope_id().to_owned();
        drop(session);
        let command = crate::ApplicationPlanDecisionCommand {
            plan_id: draft.plan_id.as_str().to_owned(),
            expected_plan_hash: draft.plan_hash.clone(),
            action,
            expected_candidate_hash: None,
            permission_grant: None,
        };
        let baseline = std::fs::read(&session_path)?;
        assert!(
            crate::application_plan_decision(&session_path, "another-session", &command).is_err()
        );
        let mut stale = command.clone();
        stale.expected_plan_hash = "stale".to_owned();
        assert!(crate::application_plan_decision(&session_path, &scope, &stale).is_err());
        assert_eq!(std::fs::read(&session_path)?, baseline);

        let receipt = crate::application_plan_decision(&session_path, &scope, &command)?;
        assert_eq!(receipt.action, action);
        let committed = std::fs::read(&session_path)?;
        let replay = crate::application_plan_decision(&session_path, &scope, &command)?;
        assert_eq!(replay.task_id, receipt.task_id);
        assert_eq!(replay.user_input_request, receipt.user_input_request);
        assert_eq!(std::fs::read(&session_path)?, committed);
        if matches!(
            action,
            crate::ApplicationPlanAction::Run | crate::ApplicationPlanAction::Reject
        ) {
            let mut forbidden = command.clone();
            forbidden.action = crate::ApplicationPlanAction::Save;
            assert!(crate::application_plan_decision(&session_path, &scope, &forbidden).is_err());
        } else if action == crate::ApplicationPlanAction::Revise {
            let mut forbidden = command.clone();
            forbidden.action = crate::ApplicationPlanAction::Run;
            assert!(crate::application_plan_decision(&session_path, &scope, &forbidden).is_err());
        }
        assert_eq!(std::fs::read(&session_path)?, committed);
    }
    Ok(())
}

#[test]
fn candidate_adoption_rejects_the_old_plan_hash_alias_without_mutating_the_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = sigil_kernel::RootConfig::load(&write_admission_test_config(temp.path()))?;
    let session_path = temp.path().join("candidate-binding.jsonl");
    let request = seed_revision_decision(&config, temp.path(), &session_path)?;
    let (_, route) = crate::provider_connections::resolve_default_model_route(&config)?;
    let inspected = crate::provider_connections::inspect_session_for_route_resume(
        &config,
        &route,
        JsonlSessionStore::new(&session_path)?,
    )?;
    let scope = inspected.session.session_scope_id().to_owned();
    drop(inspected);
    let before = std::fs::read(&session_path)?;
    let error = crate::application_plan_decision(
        &session_path,
        &scope,
        &crate::ApplicationPlanDecisionCommand {
            plan_id: request.plan_id.as_str().to_owned(),
            expected_plan_hash: format!("sha256:{}", "a".repeat(64)),
            expected_candidate_hash: None,
            action: crate::ApplicationPlanAction::AdoptCandidate,
            permission_grant: None,
        },
    )
    .expect_err("the dedicated candidate binding cannot be inferred from the plan hash");
    assert!(
        error
            .to_string()
            .contains("requires an exact candidate hash")
    );
    assert_eq!(std::fs::read(&session_path)?, before);
    Ok(())
}

#[test]
fn plan_control_ablation_targets_the_selected_plan_instead_of_the_latest_card() -> Result<()> {
    for action in [
        crate::ApplicationPlanAction::Save,
        crate::ApplicationPlanAction::Reject,
        crate::ApplicationPlanAction::Revise,
    ] {
        let (_temp, mut session, _request, draft) = durable_session_with_ready_plan()?;
        let newer = PlanReviewCoordinator::prepare_explicit_plan_review(
            &mut session,
            "Review another change",
            "newer-plan-run",
            None,
            30,
        )?;
        ensure_test_plan_review_attempt_started(&mut session, &newer, 31)?;
        let newer_draft = draft_entry(&newer);
        commit_test_plan_review_draft(&mut session, &newer_draft, &newer, 32)?;
        assert_eq!(
            public_plan_review_from_session(&session)?.plan_id,
            newer.plan_id.as_str()
        );
        let path = session.store_path().context("durable fixture")?.to_owned();
        let scope = session.session_scope_id().to_owned();
        drop(session);
        let receipt = crate::application_plan_decision(
            &path,
            &scope,
            &crate::ApplicationPlanDecisionCommand {
                plan_id: draft.plan_id.as_str().to_owned(),
                expected_plan_hash: draft.plan_hash.clone(),
                action,
                expected_candidate_hash: None,
                permission_grant: None,
            },
        )?;
        assert_eq!(receipt.plan_id, draft.plan_id.as_str());
        let current = crate::application_run::load_application_control_session(&path, &scope)?;
        assert_eq!(
            current.plan_artifact_projection().plans.get(&newer.plan_id),
            Some(&newer_draft)
        );
        assert!(
            current
                .plan_artifact_projection()
                .latest_decision(&newer.plan_id)
                .is_none()
        );
    }
    Ok(())
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
    let mut session = crate::provider_connections::load_session_for_route(
        root_config,
        &fallback_route,
        store,
        None,
        None,
        None,
    )?;
    crate::bind_session_composition(&mut session, root_config)?;
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
    commit_test_plan_review_draft(&mut session, &draft, &request, 110)?;
    let session_scope_id = session.session_scope_id().to_owned();
    drop(session);

    let receipt = crate::application_plan_decision(
        session_path,
        &session_scope_id,
        &crate::ApplicationPlanDecisionCommand {
            plan_id: request.plan_id.as_str().to_owned(),
            expected_plan_hash: draft.plan_hash.clone(),
            action: crate::ApplicationPlanAction::Revise,
            expected_candidate_hash: None,
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
    ensure_test_plan_review_attempt_started(&mut session, &request, 10)?;
    let draft = draft_entry(&request);
    commit_test_plan_review_draft(&mut session, &draft, &request, 11)?;
    Ok((session, request, draft))
}

fn durable_session_with_ready_plan() -> Result<(
    tempfile::TempDir,
    Session,
    PlanReviewRunRequest,
    PlanDraftCreatedEntry,
)> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::load_from_store("plan-review-test", "planned-model", store)?;
    let (mut session, request) = seed_route_decision(session)?;
    ensure_test_plan_review_attempt_started(&mut session, &request, 10)?;
    let draft = draft_entry(&request);
    commit_test_plan_review_draft(&mut session, &draft, &request, 11)?;
    Ok((temp, session, request, draft))
}

fn commit_revision_terminal_for_test(
    session: &mut Session,
    request: &PlanReviewRunRequest,
    outcome: &PlanReviewRunOutcome,
    now_ms: u64,
) -> Result<sigil_kernel::PublicEventOutboxEntryV1> {
    let run_id = request.child_logical_run_id();
    let event = sigil_kernel::PublicRunEvent::new(
        session.session_scope_id(),
        &run_id,
        session.next_plan_review_public_sequence(&run_id)?,
        PlanReviewCoordinator::revision_terminal_public_event(outcome)
            .context("fixture outcome must be terminal")?,
    );
    PlanReviewCoordinator::commit_revision_terminal_with_outbox(
        session, request, outcome, event, now_ms,
    )
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
    // Read the real durable stream, including direct outbox envelopes. Re-serializing only
    // SessionLogEntry would lose half the atomic terminal pair and manufacture split history.
    crate::conversation_display::conversation_display_page(
        session.store_path().context("durable review fixture")?,
        session.session_scope_id(),
        None,
        20,
        None,
    )?
    .plan_review
    .context("public plan review is missing")
}

#[test]
fn plan_control_ablation_revision_accepts_new_guidance_after_failure() -> Result<()> {
    let (_temp, mut session, _base_request, base) = durable_session_with_ready_plan()?;
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
        guidance_command.clone(),
        None,
        22,
    )?;
    assert!(replayed.idempotent_replay);
    assert_eq!(
        recovered_before_spawn.expect("durable accepted guidance must recover run authority"),
        first,
        "pre-spawn replay must recover the same physical revision attempt"
    );

    let mut objective_drift = first.clone();
    objective_drift.objective = "Preserve compatibility.".to_owned();
    let error = ensure_test_plan_review_attempt_started(&mut session, &objective_drift, 23)
        .expect_err("revision execution must retain its original objective plus accepted guidance");
    assert!(
        error
            .to_string()
            .contains("objective conflicts with its durable source binding")
    );

    ensure_test_plan_review_attempt_started(&mut session, &first, 23)?;
    let terminal = commit_revision_terminal_for_test(
        &mut session,
        &first,
        &PlanReviewRunOutcome::Failed("provider failed".to_owned()),
        24,
    )?;
    assert!(matches!(
        terminal.event.event,
        PublicRunEventKind::RunFailed { .. }
    ));
    assert_eq!(
        session
            .plan_artifact_projection()
            .latest_decision(&base.plan_id)
            .expect("revision recovery")
            .decision,
        PlanDecision::RevisionFailed
    );

    let old_attempt = PlanReviewProjection::from_entries(session.entries())
        .attempt_for_plan(&first.plan_id)
        .context("old attempt")?
        .clone();
    let old_context_digest =
        crate::plan_review_coordinator::plan_review_context_digest_for_attempt(
            &session,
            &old_attempt,
        )?;
    let next_guidance = PlanReviewCoordinator::request_plan_revision_guidance(
        &mut session,
        &base.plan_id,
        &base.plan_hash,
        25,
    )?;
    assert_eq!(
        next_guidance.request.identity.generation,
        requested.request.identity.generation + 1
    );
    let next_command = sigil_kernel::UserInputDecisionCommandV1 {
        identity: next_guidance.request.identity.clone(),
        request_hash: next_guidance.request_hash,
        command_id: sigil_kernel::UserInputCommandId::new("revision-guidance-second")?,
        decision: sigil_kernel::UserInputDecisionV1::Submitted {
            answers: vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "revision_guidance".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "Use a smaller migration with an explicit rollback step.".to_owned(),
                },
            }],
        },
    };
    let (_, retry) = PlanReviewCoordinator::accept_plan_revision_guidance(
        &mut session,
        next_command.clone(),
        None,
        26,
    )?;
    let retry = retry.expect("new guidance prepares a new revision");
    assert!(retry.objective.contains("explicit rollback step"));
    assert!(!retry.objective.contains("Preserve compatibility."));
    assert_eq!(retry.attempt_ordinal, 2);
    assert_ne!(retry.attempt_id, first.attempt_id);
    assert_eq!(retry.revision_request_id, first.revision_request_id);
    let before_replay = std::fs::read(session.store_path().context("durable fixture")?)?;
    let (_, old_replay) = PlanReviewCoordinator::accept_plan_revision_guidance(
        &mut session,
        guidance_command,
        None,
        27,
    )?;
    assert!(old_replay.is_none());
    let (_, recovered) =
        PlanReviewCoordinator::accept_plan_revision_guidance(&mut session, next_command, None, 28)?;
    assert_eq!(recovered, Some(retry.clone()));
    assert_eq!(
        std::fs::read(session.store_path().context("durable fixture")?)?,
        before_replay
    );
    let queued = public_plan_review_from_session(&session)?;
    assert!(queued.allowed_actions.is_empty());
    assert_eq!(
        queued.revision.as_ref().map(|revision| revision.status),
        Some(sigil_kernel::PublicPlanRevisionStatusV1::Queued)
    );
    assert!(
        queued
            .revision
            .as_ref()
            .is_some_and(|revision| revision.attempt_id.is_none())
    );
    ensure_test_plan_review_attempt_started(&mut session, &retry, 29)?;
    assert_eq!(
        crate::plan_review_coordinator::plan_review_context_digest_for_attempt(
            &session,
            &old_attempt,
        )?,
        old_context_digest
    );
    Ok(())
}

#[test]
fn plan_revision_queued_disables_save_until_confirmed_start_failure() -> Result<()> {
    let (_temp, mut session, _base_request, base) = durable_session_with_ready_plan()?;
    let request = submit_revision_guidance(&mut session, &base)?;
    let queued = public_plan_review_from_session(&session)?;
    assert!(queued.allowed_actions.is_empty());
    assert_eq!(
        queued.revision.as_ref().map(|revision| revision.status),
        Some(sigil_kernel::PublicPlanRevisionStatusV1::Queued)
    );
    let save = PlanDecisionCommand {
        plan_id: base.plan_id.as_str().to_owned(),
        expected_plan_hash: base.plan_hash.clone(),
        decision: PlanDecision::SavedOnly,
    };
    let queued_entries = session.entries().to_vec();
    assert!(PlanReviewCoordinator::record_plan_decision(&mut session, &save, 30).is_err());
    assert_eq!(
        serde_json::to_value(session.entries())?,
        serde_json::to_value(queued_entries)?
    );
    let failure = PlanReviewCoordinator::record_unstarted_plan_revision_failure(
        &mut session,
        &request,
        "route owner rejected this dispatch",
        31,
    )?;
    assert_eq!(failure.decision, PlanDecision::RevisionFailed);
    let failed = public_plan_review_from_session(&session)?;
    assert_eq!(failed.plan_id, base.plan_id.as_str());
    assert_eq!(failed.plan_hash.as_deref(), Some(base.plan_hash.as_str()));
    assert_eq!(
        failed.revision.as_ref().map(|revision| revision.status),
        Some(sigil_kernel::PublicPlanRevisionStatusV1::Failed)
    );
    for action in [
        sigil_kernel::PublicPlanAction::Run,
        sigil_kernel::PublicPlanAction::Save,
        sigil_kernel::PublicPlanAction::Revise,
        sigil_kernel::PublicPlanAction::Reject,
    ] {
        assert!(failed.allowed_actions.contains(&action));
    }
    assert!(
        PlanReviewProjection::from_entries(session.entries())
            .review(&request.plan_review_id)
            .expect("review")
            .attempts
            .iter()
            .all(|attempt| attempt.attempt_id != request.attempt_id)
    );
    let before_replay = session.entries().to_vec();
    assert_eq!(
        PlanReviewCoordinator::record_unstarted_plan_revision_failure(
            &mut session,
            &request,
            "same rejected dispatch",
            32
        )?,
        failure
    );
    assert_eq!(
        serde_json::to_value(session.entries())?,
        serde_json::to_value(before_replay)?
    );
    assert_eq!(
        PlanReviewCoordinator::record_plan_decision(&mut session, &save, 33)?.decision,
        PlanDecision::SavedOnly
    );
    Ok(())
}

#[test]
fn unstarted_revision_failure_cannot_settle_or_dispatch_identical_new_guidance() -> Result<()> {
    let (_temp, mut session, _base_request, base) = durable_session_with_ready_plan()?;
    let first = submit_revision_guidance(&mut session, &base)?;
    PlanReviewCoordinator::record_unstarted_plan_revision_failure(
        &mut session,
        &first,
        "first dispatch rejected before its attempt existed",
        30,
    )?;
    let failed_bytes = std::fs::read(session.store_path().context("durable fixture")?)?;
    assert!(
        PlanReviewCoordinator::ensure_revision_attempt_started(&mut session, &first, 31).is_err()
    );
    assert_eq!(
        std::fs::read(session.store_path().context("durable fixture")?)?,
        failed_bytes
    );

    let guidance = PlanReviewCoordinator::request_plan_revision_guidance(
        &mut session,
        &base.plan_id,
        &base.plan_hash,
        32,
    )?;
    let command = sigil_kernel::UserInputDecisionCommandV1 {
        identity: guidance.request.identity.clone(),
        request_hash: guidance.request_hash,
        command_id: sigil_kernel::UserInputCommandId::new("same-guidance-second-generation")?,
        decision: sigil_kernel::UserInputDecisionV1::Submitted {
            answers: vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "revision_guidance".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "Keep the public contract stable and split migration steps.".to_owned(),
                },
            }],
        },
    };
    let (_, second) = PlanReviewCoordinator::accept_plan_revision_guidance(
        &mut session,
        command.clone(),
        first.workspace_snapshot_id.clone(),
        33,
    )?;
    let second = second.context("second generation must retain run authority")?;
    assert_eq!(first.objective, second.objective);
    assert_eq!(first.workspace_snapshot_id, second.workspace_snapshot_id);
    assert_eq!(first.revision_request_id, second.revision_request_id);
    assert_eq!(
        first.attempt_ordinal, second.attempt_ordinal,
        "no attempt was persisted between generations"
    );
    assert_eq!(
        second.revision_generation,
        first.revision_generation.map(|generation| generation + 1)
    );
    assert_ne!(first.attempt_id, second.attempt_id);
    assert_ne!(first.plan_id, second.plan_id);
    assert_ne!(first.child_session_ref, second.child_session_ref);
    assert_ne!(first.child_logical_run_id(), second.child_logical_run_id());

    let before_stale = std::fs::read(session.store_path().context("durable fixture")?)?;
    assert!(
        PlanReviewCoordinator::record_unstarted_plan_revision_failure(
            &mut session,
            &first,
            "delayed first-generation failure",
            34,
        )
        .is_err()
    );
    assert!(
        PlanReviewCoordinator::ensure_revision_attempt_started(&mut session, &first, 35).is_err()
    );
    assert!(
        commit_revision_terminal_for_test(
            &mut session,
            &first,
            &PlanReviewRunOutcome::Failed("delayed terminal".to_owned()),
            36,
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(session.store_path().context("durable fixture")?)?,
        before_stale
    );
    assert_eq!(
        session
            .plan_artifact_projection()
            .latest_decision(&base.plan_id)
            .context("queued base decision")?
            .decision,
        PlanDecision::RevisionRequested
    );

    // Reload recovers the same accepted generation before worker admission.
    let store = JsonlSessionStore::new(session.store_path().context("durable fixture")?)?;
    let mut loaded = Session::load_from_store("plan-review-test", "planned-model", store)?;
    let (_, recovered) = PlanReviewCoordinator::accept_plan_revision_guidance(
        &mut loaded,
        command,
        second.workspace_snapshot_id.clone(),
        37,
    )?;
    assert_eq!(recovered, Some(second.clone()));
    PlanReviewCoordinator::ensure_revision_attempt_started(&mut loaded, &second, 38)?;
    let started_bytes = std::fs::read(loaded.store_path().context("durable fixture")?)?;
    assert!(
        PlanReviewCoordinator::record_unstarted_plan_revision_failure(
            &mut loaded,
            &first,
            "stale failure after current dispatch",
            39,
        )
        .is_err()
    );
    assert!(
        PlanReviewCoordinator::ensure_revision_attempt_started(&mut loaded, &first, 40).is_err()
    );
    assert!(
        commit_revision_terminal_for_test(
            &mut loaded,
            &first,
            &PlanReviewRunOutcome::Failed("stale terminal after dispatch".to_owned()),
            41,
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(loaded.store_path().context("durable fixture")?)?,
        started_bytes
    );
    let reviews = PlanReviewProjection::from_entries(loaded.entries());
    assert_eq!(
        reviews
            .latest_attempt(&second.plan_review_id)
            .context("current started attempt")?
            .attempt_id,
        second.attempt_id
    );
    commit_revision_terminal_for_test(
        &mut loaded,
        &second,
        &PlanReviewRunOutcome::Failed("current generation finished".to_owned()),
        42,
    )?;
    assert_eq!(
        loaded
            .plan_artifact_projection()
            .latest_decision(&base.plan_id)
            .context("current terminal decision")?
            .decision,
        PlanDecision::RevisionFailed
    );
    Ok(())
}

#[test]
fn historical_revision_attempt_recovers_generation_without_rewriting_its_identity() -> Result<()> {
    let (_temp, mut session, _base_request, base) = durable_session_with_ready_plan()?;
    let mut historical = submit_revision_guidance(&mut session, &base)?;
    historical.attempt_id = sigil_kernel::PlanReviewAttemptId::new("historical-revision-attempt")?;
    historical.plan_id = sigil_kernel::PlanId::new("historical-revision-plan")?;
    historical.child_session_ref = SessionRef::new_relative("historical-revision-research.jsonl")?;
    let historical_entry = sigil_kernel::PlanReviewAttemptEntry {
        plan_review_id: historical.plan_review_id.clone(),
        attempt_id: historical.attempt_id.clone(),
        plan_id: historical.plan_id.clone(),
        source: historical.source,
        source_turn: historical.source_turn.clone(),
        route_decision_id: historical.route_decision_id.clone(),
        child_session_ref: historical.child_session_ref.clone(),
        revision_request_id: historical.revision_request_id.clone(),
        attempt_ordinal: historical.attempt_ordinal,
        base_plan_id: historical.base_plan_id.clone(),
        base_plan_hash: historical.base_plan_hash.clone(),
        explicit_objective: historical.explicit_objective.clone(),
        workspace_snapshot_id: historical.workspace_snapshot_id.clone(),
        pending_user_input: None,
        status: PlanReviewAttemptStatus::Started,
        terminal_reason: None,
        recorded_at_ms: 30,
    };
    assert!(
        serde_json::to_value(&historical_entry)?
            .get("revision_generation")
            .is_none()
    );
    session.append_control(ControlEntry::PlanReviewAttempt(historical_entry.clone()))?;
    let store = JsonlSessionStore::new(session.store_path().context("durable fixture")?)?;
    // A concurrent control reader must preserve this active attempt; startup loading would
    // intentionally reconcile it as interrupted before the historical replay assertions.
    let mut loaded = Session::load_from_store_for_control(store)?;
    let before = std::fs::read(loaded.store_path().context("durable fixture")?)?;
    PlanReviewCoordinator::ensure_revision_attempt_started(&mut loaded, &historical, 31)?;
    let digest = crate::plan_review_coordinator::plan_review_context_digest_for_attempt(
        &loaded,
        &historical_entry,
    )?;
    assert_eq!(
        std::fs::read(loaded.store_path().context("durable fixture")?)?,
        before
    );
    commit_revision_terminal_for_test(
        &mut loaded,
        &historical,
        &PlanReviewRunOutcome::Failed("historical failure".to_owned()),
        32,
    )?;
    let new_guidance = PlanReviewCoordinator::request_plan_revision_guidance(
        &mut loaded,
        &base.plan_id,
        &base.plan_hash,
        33,
    )?;
    assert_eq!(new_guidance.request.identity.generation, 2);
    assert_eq!(
        crate::plan_review_coordinator::plan_review_context_digest_for_attempt(
            &loaded,
            &historical_entry,
        )?,
        digest,
        "later guidance cannot rebind the historical attempt prefix"
    );
    Ok(())
}

#[test]
fn plan_revision_start_failure_rejects_started_or_changed_attempts() -> Result<()> {
    let (_temp, mut session, _base_request, base) = durable_session_with_ready_plan()?;
    let request = submit_revision_guidance(&mut session, &base)?;
    let mut changed = request.clone();
    changed.attempt_ordinal += 1;
    let before = session.entries().to_vec();
    assert!(
        PlanReviewCoordinator::record_unstarted_plan_revision_failure(
            &mut session,
            &changed,
            "stale owner",
            30
        )
        .is_err()
    );
    assert_eq!(
        serde_json::to_value(session.entries())?,
        serde_json::to_value(before)?
    );
    ensure_test_plan_review_attempt_started(&mut session, &request, 31)?;
    let started = session.entries().to_vec();
    assert!(
        PlanReviewCoordinator::record_unstarted_plan_revision_failure(
            &mut session,
            &request,
            "too late",
            32
        )
        .is_err()
    );
    assert_eq!(
        serde_json::to_value(session.entries())?,
        serde_json::to_value(started)?
    );
    assert_eq!(
        session
            .plan_artifact_projection()
            .latest_decision(&base.plan_id)
            .expect("decision")
            .decision,
        PlanDecision::RevisionRequested
    );
    Ok(())
}

#[test]
fn plan_revision_terminal_without_draft_preserves_status_and_restores_the_base_plan() -> Result<()>
{
    let outcomes = [
        PlanReviewRunOutcome::Failed("failed".to_owned()),
        PlanReviewRunOutcome::Interrupted("interrupted".to_owned()),
        PlanReviewRunOutcome::Cancelled,
        PlanReviewRunOutcome::CompletedWithoutDraft,
        PlanReviewRunOutcome::Blocked("blocked".to_owned()),
        PlanReviewRunOutcome::Paused("paused".to_owned()),
    ];
    for (index, outcome) in outcomes.into_iter().enumerate() {
        let (_temp, mut session, _base_request, base) = durable_session_with_ready_plan()?;
        let revision = submit_revision_guidance(&mut session, &base)?;
        ensure_test_plan_review_attempt_started(&mut session, &revision, 30 + index as u64)?;
        let expected_event = PlanReviewCoordinator::revision_terminal_public_event(&outcome)
            .context("terminal outcome")?;
        let terminal = commit_revision_terminal_for_test(
            &mut session,
            &revision,
            &outcome,
            40 + index as u64,
        )?;
        assert_eq!(
            serde_json::to_value(&terminal.event.event)?,
            serde_json::to_value(expected_event)?,
            "base RevisionFailed must not collapse the actual run status"
        );
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
    let (_temp, mut session, _base_request, base) = durable_session_with_ready_plan()?;
    let revision = submit_revision_guidance(&mut session, &base)?;
    ensure_test_plan_review_attempt_started(&mut session, &revision, 30)?;
    let mut revised = draft_entry(&revision);
    revised.summary = "Revised migration".to_owned();
    let terminal = commit_revision_terminal_for_test(
        &mut session,
        &revision,
        &PlanReviewRunOutcome::DraftReady {
            draft: Box::new(revised.clone()),
        },
        31,
    )?;
    assert!(matches!(
        terminal.event.event,
        PublicRunEventKind::RunFinished { .. }
    ));
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
            let _ = socket.read(&mut buffer).await;
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"revision-draft-call\",\"type\":\"function\",\"function\":{\"name\":\"submit_plan_review_result\",\"arguments\":\"{\\\"schema_version\\\":1,\\\"outcome\\\":\\\"draft\\\",\\\"content\\\":\\\"# Revised coordinator migration\\\\n\\\\n1. Revise migration.\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n"
            );
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
    let storage = isolated_storage_toml(&config_path);
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

[workspace]
root = "."

{storage}

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
    let execution = crate::application_run::execute_plan_review_revision_with_managed_execution(
        &root_config,
        &workspace_root,
        &session_path,
        &revision_request,
        &mut handler,
        None,
        None,
        None,
        None,
    )
    .await?;
    let outbox = execution
        .terminal_outbox
        .expect("revision must return its exact durable terminal outbox");
    let outcome = execution.outcome;
    let PlanReviewRunOutcome::DraftReady {
        draft: revised_draft,
    } = outcome
    else {
        panic!("revision ordinary tool loop must commit a typed draft: {outcome:?}");
    };
    assert_eq!(revised_draft.summary, "Revised coordinator migration");
    assert!(!handler.0.is_empty(), "revision run must publish events");
    assert_eq!(
        outbox.event.sequence,
        handler
            .0
            .last()
            .expect("revision must publish a nonterminal bridge event")
            .sequence
            .saturating_add(1),
        "the terminal bundle must reserve the bridge's next exact sequence"
    );

    // The durable session now has the revision attempt in DraftReady with the new plan bound.
    let store = sigil_kernel::JsonlSessionStore::new(&session_path)?;
    let (_, fallback_route) =
        crate::provider_connections::resolve_default_model_route(&root_config)?;
    let reloaded = crate::provider_connections::load_session_for_route(
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
    let records = JsonlSessionStore::new(&session_path)?.read_event_records_writer()?;
    let outbox_projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    let persisted = outbox_projection
        .events_in_order()
        .into_iter()
        .find(|entry| entry.public_event_id == outbox.public_event_id)
        .context("revision terminal bundle is missing its original outbox")?;
    assert_eq!(
        serde_json::to_value(&persisted.event)?,
        serde_json::to_value(&outbox.event)?
    );
    fixture.abort();
    Ok(())
}

#[tokio::test]
async fn plan_control_ablation_removed_connection_settles_revision_and_restores_base_actions()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = write_admission_test_config(temp.path());
    let listener = TokioTcpListener::bind("127.0.0.1:0").await?;
    let original = std::fs::read_to_string(&config_path)?.replace(
        "http://127.0.0.1:1",
        &format!("http://{}", listener.local_addr()?),
    );
    let config: sigil_kernel::RootConfig = toml::from_str(&original)?;
    let session_path = temp.path().join("revision-removed-connection.jsonl");
    let request = seed_revision_decision(&config, temp.path(), &session_path)?;
    let changed: sigil_kernel::RootConfig =
        toml::from_str(&original.replace("local-test", "replacement"))?;
    let mut handler = RecordingRevisionHandler(Vec::new());
    let execution = crate::application_run::execute_plan_review_revision_with_managed_execution(
        &changed,
        temp.path(),
        &session_path,
        &request,
        &mut handler,
        None,
        None,
        None,
        None,
    )
    .await?;
    assert!(matches!(execution.outcome, PlanReviewRunOutcome::Failed(_)));
    assert!(execution.terminal_outbox.is_some());
    let session = crate::application_run::load_application_control_session(
        &session_path,
        &request.source_turn.session_scope_id,
    )?;
    let public = public_plan_review_from_session(&session)?;
    assert_eq!(
        public.plan_id,
        request.base_plan_id.as_ref().context("base plan")?.as_str()
    );
    for action in [
        sigil_kernel::PublicPlanAction::Run,
        sigil_kernel::PublicPlanAction::Save,
        sigil_kernel::PublicPlanAction::Revise,
        sigil_kernel::PublicPlanAction::Reject,
    ] {
        assert!(public.allowed_actions.contains(&action));
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept(),)
            .await
            .is_err(),
        "missing original connection must not dispatch to the replacement route"
    );
    let before = std::fs::read(&session_path)?;
    let replay = crate::application_run::execute_plan_review_revision_with_managed_execution(
        &changed,
        temp.path(),
        &session_path,
        &request,
        &mut handler,
        None,
        None,
        None,
        None,
    )
    .await?;
    assert_eq!(
        serde_json::to_value(&replay.terminal_outbox)?,
        serde_json::to_value(&execution.terminal_outbox)?,
    );
    assert_eq!(std::fs::read(&session_path)?, before);
    Ok(())
}

#[tokio::test]
async fn revision_provider_construction_failure_commits_confirmed_failed_terminal() -> Result<()> {
    assert_revision_provider_construction_failure(false).await
}

#[tokio::test]
async fn revision_provider_construction_failure_preserves_requested_cancellation() -> Result<()> {
    assert_revision_provider_construction_failure(true).await
}

async fn assert_revision_provider_construction_failure(cancel_requested: bool) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let session_path = temp.path().join("session.jsonl");
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
credential = {{ source = "none" }}
"#
        ),
    )?;
    let root_config: sigil_kernel::RootConfig =
        toml::from_str(&std::fs::read_to_string(&config_path)?)?;
    let workspace_root = sigil_kernel::resolve_workspace_root(&config_path, &workspace, ".");
    let revision_request = seed_revision_decision(&root_config, &workspace_root, &session_path)?;

    let mut recorded = Vec::new();
    let mut handler = RecordingRevisionHandler(std::mem::take(&mut recorded));
    let cancellation = RunCancellationOwner::new();
    if cancel_requested {
        assert!(cancellation.request_cancel());
    }
    let execution = crate::application_run::execute_plan_review_revision_with_managed_execution(
        &root_config,
        &workspace_root,
        &session_path,
        &revision_request,
        &mut handler,
        Some(cancellation.handle()),
        None,
        None,
        None,
    )
    .await?;
    let outcome = execution.outcome;
    if cancel_requested {
        assert!(matches!(outcome, PlanReviewRunOutcome::Cancelled));
        let terminal = execution
            .terminal_outbox
            .context("durable cancellation terminal")?;
        assert!(matches!(
            terminal.event.event,
            PublicRunEventKind::RunCancelled
        ));
        let records = JsonlSessionStore::new(&session_path)?.read_event_records_writer()?;
        let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
        let events = outbox.events_in_order();
        assert_eq!(
            events
                .iter()
                .filter(|entry| entry.run_id == revision_request.child_logical_run_id())
                .filter(|entry| matches!(entry.event.event, PublicRunEventKind::RunCancelled))
                .count(),
            1
        );
        assert!(
            !events
                .iter()
                .filter(|entry| entry.run_id == revision_request.child_logical_run_id())
                .any(|entry| matches!(entry.event.event, PublicRunEventKind::RunFailed { .. }))
        );
    } else {
        assert!(matches!(outcome, PlanReviewRunOutcome::Failed(_)));
        assert!(cancellation.handle().is_naturally_finalized());
        assert!(
            !cancellation.request_cancel(),
            "a settled failure cannot become cancelled"
        );
    }

    // Provider construction failed before dispatch; the exact cause is a confirmed failure.
    let store = sigil_kernel::JsonlSessionStore::new(&session_path)?;
    let (_, fallback_route) =
        crate::provider_connections::resolve_default_model_route(&root_config)?;
    let reloaded = crate::provider_connections::load_session_for_route(
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
        if cancel_requested {
            PlanReviewAttemptStatus::Cancelled
        } else {
            PlanReviewAttemptStatus::Failed
        },
        "confirmed zero-dispatch failure must use the same cancellation arbitration as a run"
    );
    assert_eq!(
        attempt.terminal_reason,
        Some(if cancel_requested {
            sigil_kernel::PlanReviewTerminalReason::UserCancelled
        } else {
            sigil_kernel::PlanReviewTerminalReason::RunFailed
        })
    );
    Ok(())
}

#[tokio::test]
async fn cancelling_suspended_revision_commits_the_original_terminal_outbox() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let session_path = temp.path().join("session.jsonl");
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
connection = "local-test"
model = "gpt-4.1"
tool_timeout_secs = 5

[task]
routing_policy = "auto"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:9"
credential = {{ source = "none" }}
"#
        ),
    )?;
    let root_config: sigil_kernel::RootConfig =
        toml::from_str(&std::fs::read_to_string(&config_path)?)?;
    let workspace_root = sigil_kernel::resolve_workspace_root(&config_path, &workspace, ".");
    let request = seed_revision_decision(&root_config, &workspace_root, &session_path)?;
    let store = JsonlSessionStore::new(&session_path)?;
    let (_, fallback_route) =
        crate::provider_connections::resolve_default_model_route(&root_config)?;
    let mut session = crate::provider_connections::load_session_for_route(
        &root_config,
        &fallback_route,
        store,
        None,
        None,
        None,
    )?;
    ensure_test_plan_review_attempt_started(&mut session, &request, 200)?;
    let provider = AskingPlanReviewProvider::default();
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut handler = NoopEventHandler;
    let mut approvals = AutoApproveHandler;
    let PlanReviewRunOutcome::AwaitingUserInput { request: pending } =
        PlanReviewCoordinator::run_plan_review(
            &mut session,
            &request,
            &agent,
            plan_review_test_options(temp.path()),
            ToolRegistry::new(),
            &mut handler,
            &mut approvals,
            RunCancellationOwner::new().handle(),
        )
        .await?
    else {
        panic!("revision research must suspend for a durable question");
    };
    assert!(
        close_test_plan_review_run(
            &mut session,
            &request,
            &PlanReviewRunOutcome::AwaitingUserInput {
                request: pending.clone(),
            },
            210,
        )
        .is_err()
    );
    let run_id = request.child_logical_run_id();
    let waiting_event = sigil_kernel::PublicRunEvent::new(
        session.session_scope_id(),
        &run_id,
        session.next_plan_review_public_sequence(&run_id)?,
        sigil_kernel::PublicRunEventKind::RunAwaitingUserInput {
            request_id: pending.identity.request_id.as_str().to_owned(),
            generation: pending.identity.generation,
            request_hash: pending.request_hash.clone(),
        },
    );
    let waiting_outbox = PlanReviewCoordinator::commit_revision_waiting_with_outbox(
        &mut session,
        &request,
        &pending,
        waiting_event,
        210,
    )?;
    assert!(matches!(
        PlanReviewCoordinator::revision_waiting_outcome_from_outbox(
            &session,
            &request,
            &waiting_outbox,
        )?,
        PlanReviewRunOutcome::AwaitingUserInput { .. }
    ));
    let waiting_records = JsonlSessionStore::new(&session_path)?.read_event_records_writer()?;
    let waiting_projection =
        sigil_kernel::PublicEventOutboxProjectionV1::from_records(&waiting_records)?;
    assert!(
        waiting_projection
            .pending_for_adapter("application")
            .iter()
            .any(|entry| entry.public_event_id == waiting_outbox.public_event_id),
        "the revision Waiting notification remains durable-but-unpublished before cancellation"
    );
    let (receipt, resumed, outbox) = PlanReviewCoordinator::accept_plan_review_research_input(
        &mut session,
        sigil_kernel::UserInputDecisionCommandV1 {
            identity: pending.identity.clone(),
            request_hash: pending.request_hash.clone(),
            command_id: sigil_kernel::UserInputCommandId::new("cancel-revision-research")?,
            decision: sigil_kernel::UserInputDecisionV1::RunCancelled,
        },
        220,
    )?;
    assert!(matches!(
        receipt.request.resolution,
        Some(sigil_kernel::UserInputResolutionV1::RunCancelled)
    ));
    assert!(
        resumed.is_none(),
        "cancelled research must not start another child"
    );
    let outbox = outbox.expect("revision cancellation must produce the durable terminal outbox");
    assert!(matches!(
        &outbox.event.event,
        PublicRunEventKind::RunCancelled
    ));
    assert_eq!(outbox.run_id, request.child_logical_run_id());
    assert_eq!(
        outbox.sequence,
        waiting_outbox.event.sequence + 1,
        "the cancelled terminal advances only from the durable Waiting outbox frontier"
    );
    let projection = PlanReviewProjection::from_entries(session.entries());
    let attempt = projection
        .latest_attempt(&request.plan_review_id)
        .expect("cancelled revision attempt");
    assert_eq!(attempt.status, PlanReviewAttemptStatus::Cancelled);
    let records = JsonlSessionStore::new(&session_path)?.read_event_records_writer()?;
    let outbox_projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    assert!(
        outbox_projection
            .events_in_order()
            .into_iter()
            .any(|entry| {
                entry.public_event_id == outbox.public_event_id
                    && serde_json::to_value(&entry.event).ok()
                        == serde_json::to_value(&outbox.event).ok()
            })
    );
    let (replayed, replay_resumed, replay_outbox) =
        PlanReviewCoordinator::accept_plan_review_research_input(
            &mut session,
            sigil_kernel::UserInputDecisionCommandV1 {
                identity: pending.identity,
                request_hash: pending.request_hash,
                command_id: sigil_kernel::UserInputCommandId::new("cancel-revision-research")?,
                decision: sigil_kernel::UserInputDecisionV1::RunCancelled,
            },
            230,
        )?;
    assert!(replayed.idempotent_replay);
    assert!(replay_resumed.is_none());
    assert!(replay_outbox.is_none());
    Ok(())
}

#[tokio::test]
async fn revision_draft_conflict_is_rejected_before_start_without_a_fake_terminal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let session_path = temp.path().join("session.jsonl");

    let (fixture, base_url) = spawn_revision_draft_fixture().await?;
    let config_path = temp.path().join("sigil.toml");
    let storage = isolated_storage_toml(&config_path);
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

[workspace]
root = "."

{storage}

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
    let mut session = crate::provider_connections::load_session_for_route(
        &root_config,
        &fallback_route,
        store,
        None,
        None,
        None,
    )?;
    let mut conflicting = draft_entry(&revision_request);
    conflicting.summary = "conflicting pre-existing draft".to_owned();
    session.append_control(sigil_kernel::ControlEntry::PlanDraftCreated(
        conflicting.clone(),
    ))?;
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
        "pre-committed conflicting draft must reject revision admission"
    );
    assert!(
        handler.0.is_empty(),
        "rejected admission must not emit a public run event"
    );

    // The new atomic boundary rejects this conflict before Started, rather than admitting a
    // split candidate and inventing a Failed terminal afterwards. The prior base attempt stays
    // intact and the accepted guidance remains a recovery candidate.
    let store = sigil_kernel::JsonlSessionStore::new(&session_path)?;
    let reloaded = crate::provider_connections::load_session_for_route(
        &root_config,
        &fallback_route,
        store,
        None,
        None,
        None,
    )?;
    let projection = sigil_kernel::PlanReviewProjection::from_entries(reloaded.entries());
    assert!(
        projection
            .review(&revision_request.plan_review_id)
            .expect("base review")
            .attempts
            .iter()
            .all(|attempt| attempt.attempt_id != revision_request.attempt_id),
        "pre-start rejection must not fabricate any revision attempt"
    );
    assert_eq!(
        reloaded
            .plan_artifact_projection()
            .plans
            .get(&conflicting.plan_id),
        Some(&conflicting)
    );
    assert_eq!(
        reloaded
            .plan_artifact_projection()
            .latest_decision(revision_request.base_plan_id.as_ref().expect("base plan"))
            .expect("accepted guidance decision")
            .decision,
        PlanDecision::RevisionRequested
    );
    let records = JsonlSessionStore::read_event_records(&session_path)?;
    assert!(
        sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?
            .events_in_order()
            .iter()
            .all(|entry| entry.run_id != revision_request.child_logical_run_id())
    );
    fixture.abort();
    Ok(())
}

#[test]
fn prepare_automatic_plan_review_validates_without_starting_attempt() -> Result<()> {
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

    assert!(
        PlanReviewProjection::from_entries(session.entries())
            .latest_attempt(&request.plan_review_id)
            .is_none(),
        "prepare must not leave a recoverable Started attempt before an executor is admitted"
    );

    let mut handler = RecordingPlanReviewEvents::default();
    PlanReviewCoordinator::ensure_attempt_started(&mut session, &prepared, &mut handler, 100)?;
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

    // Idempotent preparation still has no lifecycle side effect, and execution commits once.
    let again =
        PlanReviewCoordinator::prepare_automatic_plan_review(&mut session, &action, None, 101)?;
    assert_eq!(again.attempt_id, request.attempt_id);
    PlanReviewCoordinator::ensure_attempt_started(&mut session, &again, &mut handler, 101)?;
    let projection = PlanReviewProjection::from_entries(session.entries());
    assert_eq!(
        projection
            .review(&request.plan_review_id)
            .expect("review")
            .attempts
            .len(),
        1
    );
    assert_eq!(
        handler
            .0
            .iter()
            .filter(|event| matches!(
                event,
                RunEvent::Control(ControlEntry::PlanReviewAttempt(attempt))
                    if attempt.status == PlanReviewAttemptStatus::Started
            ))
            .count(),
        1,
        "the parent Started transition crosses the handler commit boundary exactly once"
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
    assert_eq!(
        prepared.explicit_objective.as_deref(),
        Some("draft the RFC"),
        "an explicit command has no durable user turn, so its safe source objective belongs to the attempt"
    );
    assert!(
        PlanReviewProjection::from_entries(session.entries())
            .latest_attempt(&prepared.plan_review_id)
            .is_none(),
        "explicit preparation must not create a Started attempt before the root application bridge"
    );
    let mut handler = RecordingPlanReviewEvents::default();
    PlanReviewCoordinator::ensure_attempt_started(&mut session, &prepared, &mut handler, 100)?;
    let projection = PlanReviewProjection::from_entries(session.entries());
    let attempt = projection
        .latest_attempt(&prepared.plan_review_id)
        .expect("attempt");
    assert_eq!(attempt.status, PlanReviewAttemptStatus::Started);
    assert_eq!(attempt.source, PlanReviewSource::ExplicitPlanCommand);
    assert_eq!(attempt.explicit_objective.as_deref(), Some("draft the RFC"));

    let mut objective_drift = prepared.clone();
    objective_drift.objective = "draft a different RFC".to_owned();
    let error = PlanReviewCoordinator::ensure_attempt_started(
        &mut session,
        &objective_drift,
        &mut handler,
        101,
    )
    .expect_err("the transient objective must not drift from its durable explicit binding");
    assert!(
        error
            .to_string()
            .contains("objective conflicts with its durable source binding")
    );

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

    commit_test_plan_review_draft(&mut session, &draft, &request, 110)?;
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
    commit_test_plan_review_draft(&mut session, &draft, &request, 111)?;
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
            &mut NoopEventHandler,
            112,
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn commit_draft_keeps_parent_draft_and_attempt_in_one_handler_batch() -> Result<()> {
    let (mut session, request) = session_with_route_decision()?;
    let action = sigil_kernel::StartPlanReviewAction {
        decision_id: request.route_decision_id.clone().expect("decision id"),
        plan_review_id: request.plan_review_id.clone(),
        plan_id: request.plan_id.clone(),
        source_turn: request.source_turn.clone(),
    };
    let request =
        PlanReviewCoordinator::prepare_automatic_plan_review(&mut session, &action, None, 100)?;
    let draft = draft_entry(&request);
    let mut handler = RecordingPlanReviewControlBatches::default();

    PlanReviewCoordinator::ensure_attempt_started(&mut session, &request, &mut handler, 101)?;
    PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &mut handler,
        102,
    )?;

    assert_eq!(handler.batches.len(), 2);
    assert_eq!(handler.batches[0].len(), 1);
    let ControlEntry::PlanReviewAttempt(started) = &handler.batches[0][0] else {
        panic!("the first parent commit must contain Started only");
    };
    assert_eq!(started.status, PlanReviewAttemptStatus::Started);
    assert_eq!(handler.batches[1].len(), 2);
    let ControlEntry::PlanDraftCreated(recorded) = &handler.batches[1][0] else {
        panic!("the draft settlement must retain PlanDraftCreated in its parent batch");
    };
    let ControlEntry::PlanReviewAttempt(ready) = &handler.batches[1][1] else {
        panic!("the draft settlement must retain PlanReviewAttempt in its parent batch");
    };
    assert_eq!(recorded, &draft);
    assert_eq!(ready.status, PlanReviewAttemptStatus::DraftReady);
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
    complete_test_plan_review_without_draft(&mut session, &request, 120)?;
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
    assert!(complete_test_plan_review_without_draft(&mut session, &request, 121).is_ok());
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
    commit_test_plan_review_draft(&mut session, &draft, &request, 110)?;

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
fn historical_task_creation_failure_remains_reviewable_after_reload() -> Result<()> {
    let (mut session, _request, draft) = session_with_ready_plan()?;
    let failure = sigil_kernel::PlanDecisionRecordedEntry {
        plan_id: draft.plan_id.clone(),
        plan_hash: draft.plan_hash.clone(),
        decision: PlanDecision::TaskCreationFailed,
        decided_by: sigil_kernel::PlanDecisionActor::System,
        decided_at_ms: 120,
        reason: Some("plan has no concrete target paths for scoped edits".to_owned()),
    };
    session.append_control(ControlEntry::PlanDecisionRecorded(failure.clone()))?;
    let entries: Vec<SessionLogEntry> =
        serde_json::from_str(&serde_json::to_string(session.entries())?)?;
    let projection = sigil_kernel::PlanArtifactProjection::from_entries(&entries);
    assert_eq!(projection.latest_decision(&draft.plan_id), Some(&failure));
    assert_eq!(projection.latest_pending_plan(), Some(&draft));
    assert!(
        sigil_kernel::TaskStateProjection::from_entries(&entries)
            .tasks
            .is_empty()
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
    commit_test_plan_review_draft(&mut session, &draft, &request, 110)?;

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

    let command = sigil_kernel::PlanRunCommandV1 {
        command_id: "rejected-plan-run".to_owned(),
        session_id: session.session_scope_id().to_owned(),
        plan_id: draft.plan_id.clone(),
        expected_plan_hash: draft.plan_hash,
        expected_durable_frontier: session.durable_frontier_sequence(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreatePaused,
        permission: sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy,
        source: sigil_kernel::PlanRunCommandSource::Http,
    };
    assert!(matches!(
        crate::PlanExecutionService::approve(
            &mut session,
            SessionRef::new_relative("session.jsonl")?,
            &command,
            120,
        ),
        Err(sigil_kernel::PlanRunRejectionV1::PlanRejected)
            | Err(sigil_kernel::PlanRunRejectionV1::PlanNotReady)
    ));
    assert!(session.task_state_projection().tasks.is_empty());
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
    let storage = isolated_storage_toml(&path);
    std::fs::create_dir_all(root).expect("test root");
    std::fs::write(
        &path,
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

[task]
enabled = true
"#
        ),
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

fn mark_r69_plan_reviewable(session: &mut Session, draft: &PlanDraftCreatedEntry) -> Result<()> {
    let mut attempt = sigil_kernel::PlanReviewAttemptEntry {
        plan_review_id: sigil_kernel::PlanReviewId::new(format!(
            "review-{}",
            draft.plan_id.as_str()
        ))?,
        attempt_id: sigil_kernel::PlanReviewAttemptId::new(format!(
            "attempt-{}",
            draft.plan_id.as_str()
        ))?,
        plan_id: draft.plan_id.clone(),
        source: PlanReviewSource::ExplicitPlanCommand,
        source_turn: ConversationTurnRef {
            session_scope_id: session.session_scope_id().to_owned(),
            message_id: format!("message-{}", draft.plan_id.as_str()),
            logical_run_id: format!("run-{}", draft.plan_id.as_str()),
        },
        route_decision_id: None,
        child_session_ref: SessionRef::new_relative("child-reviewable.jsonl")?,
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        explicit_objective: Some("reviewable plan".to_owned()),
        workspace_snapshot_id: None,
        pending_user_input: None,
        status: PlanReviewAttemptStatus::Started,
        terminal_reason: None,
        recorded_at_ms: 29,
    };
    session.append_control(ControlEntry::PlanReviewAttempt(attempt.clone()))?;
    attempt.status = PlanReviewAttemptStatus::DraftReady;
    attempt.recorded_at_ms = 30;
    session.append_control(ControlEntry::PlanReviewAttempt(attempt))?;
    Ok(())
}

#[test]
fn plan_control_ablation_approval_replay_preserves_execution_progress() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("approval-progress.jsonl"))?;
    let (mut session, request) =
        seed_route_decision(Session::load_from_store("test", "model", store)?)?;
    ensure_test_plan_review_attempt_started(&mut session, &request, 10)?;
    let mut draft = draft_entry(&request);
    let mut second = draft.steps[0].clone();
    second.step_id = "verify".to_owned();
    second.title = "Verify the result".to_owned();
    draft.steps.push(second);
    commit_test_plan_review_draft(&mut session, &draft, &request, 11)?;
    let parent = SessionRef::new_relative("approval-progress.jsonl")?;
    let command = sigil_kernel::PlanRunCommandV1 {
        command_id: "approval-progress".to_owned(),
        session_id: session.session_scope_id().to_owned(),
        plan_id: draft.plan_id.clone(),
        expected_plan_hash: draft.plan_hash.clone(),
        expected_durable_frontier: session.durable_frontier_sequence(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreateAndRun,
        permission: sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy,
        source: sigil_kernel::PlanRunCommandSource::Http,
    };
    let approved = crate::PlanExecutionService::approve(&mut session, parent.clone(), &command, 12)
        .map_err(|error| anyhow!(crate::plan_run_rejection_message(&error)))?;
    let task = session
        .task_state_projection()
        .tasks
        .get(&approved.task_id)
        .context("approved task")?
        .clone();
    let mut update = task
        .checklist
        .as_ref()
        .context("two-step checklist")?
        .clone();
    update.revision += 1;
    update.items[0].status = sigil_kernel::TaskChecklistItemStatusV1::InProgress;
    update.validate()?;
    session.append_control(ControlEntry::TaskChecklistUpdatedV1(update.clone()))?;
    let before = std::fs::read(session.store_path().context("durable fixture")?)?;
    let replay = crate::PlanExecutionService::approve(&mut session, parent.clone(), &command, 13)
        .map_err(|error| anyhow!(crate::plan_run_rejection_message(&error)))?;
    assert_eq!(replay.task_id, approved.task_id);
    assert!(replay.already_approved);
    assert_eq!(
        session.task_state_projection().tasks[&approved.task_id]
            .checklist
            .as_ref(),
        Some(&update)
    );
    let mut changed_permission = command.clone();
    changed_permission.permission = sigil_kernel::PlanRunPermissionChoiceV1::GrantScopedEditsOnce;
    assert!(matches!(
        crate::PlanExecutionService::approve(&mut session, parent, &changed_permission, 14),
        Err(sigil_kernel::PlanRunRejectionV1::CommandIdentityConflict)
    ));
    assert_eq!(
        std::fs::read(session.store_path().context("durable fixture")?)?,
        before
    );
    Ok(())
}

#[test]
fn approved_plan_is_directly_executable_without_legacy_materialization() -> Result<()> {
    let (mut session, draft, _temp) = spine_session("plan_r69_materialization", "Materialize")?;
    let mut review_attempt = sigil_kernel::PlanReviewAttemptEntry {
        plan_review_id: sigil_kernel::PlanReviewId::new("review-r69-materialization")?,
        attempt_id: sigil_kernel::PlanReviewAttemptId::new("attempt-r69-materialization")?,
        plan_id: draft.plan_id.clone(),
        source: PlanReviewSource::ExplicitPlanCommand,
        source_turn: ConversationTurnRef {
            session_scope_id: session.session_scope_id().to_owned(),
            message_id: "message-r69-materialization".to_owned(),
            logical_run_id: "run-r69-materialization".to_owned(),
        },
        route_decision_id: None,
        child_session_ref: SessionRef::new_relative("child-r69-materialization.jsonl")?,
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        explicit_objective: Some("materialize plan".to_owned()),
        workspace_snapshot_id: None,
        pending_user_input: None,
        status: PlanReviewAttemptStatus::Started,
        terminal_reason: None,
        recorded_at_ms: 29,
    };
    session.append_control(ControlEntry::PlanReviewAttempt(review_attempt.clone()))?;
    review_attempt.status = PlanReviewAttemptStatus::DraftReady;
    review_attempt.recorded_at_ms = 30;
    session.append_control(ControlEntry::PlanReviewAttempt(review_attempt))?;
    let command = sigil_kernel::PlanRunCommandV1 {
        command_id: "r69-materialize-command".to_owned(),
        session_id: session.session_scope_id().to_owned(),
        plan_id: draft.plan_id.clone(),
        expected_plan_hash: draft.plan_hash.clone(),
        expected_durable_frontier: session.durable_frontier_sequence(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreateAndRun,
        permission: sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy,
        source: sigil_kernel::PlanRunCommandSource::Http,
    };
    let approval = crate::PlanExecutionService::approve(
        &mut session,
        SessionRef::new_relative("parent.jsonl")?,
        &command,
        30,
    )
    .map_err(|rejection| anyhow::anyhow!(crate::plan_run_rejection_message(&rejection)))?;
    let artifacts = session.plan_artifact_projection();
    assert!(artifacts.task_created_for_plan(&draft.plan_id));
    let task = session.task_state_projection();
    let projected = task.tasks.get(&approval.task_id).expect("approved Task");
    assert!(projected.plans.is_empty());
    assert!(projected.direct_execution_admission.is_some());
    Ok(())
}

#[test]
fn plan_execution_service_rejects_typed_without_consuming_the_plan() -> Result<()> {
    let (mut session, draft, _temp) = spine_session("plan_spine_2", "Reject typed")?;
    mark_r69_plan_reviewable(&mut session, &draft)?;
    let parent_ref = SessionRef::new_relative("parent.jsonl")?;
    let base = sigil_kernel::PlanRunCommandV1 {
        command_id: "run-command-reject".to_owned(),
        session_id: session.session_scope_id().to_owned(),
        plan_id: draft.plan_id.clone(),
        expected_plan_hash: draft.plan_hash.clone(),
        expected_durable_frontier: session.durable_frontier_sequence(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreateAndRun,
        permission: sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy,
        source: sigil_kernel::PlanRunCommandSource::Http,
    };
    // A stale approved Plan hash leaves the current Plan actionable.
    let mut stale_hash = base.clone();
    stale_hash.expected_plan_hash = "sha256:stale".to_owned();
    let rejection = match crate::PlanExecutionService::approve(
        &mut session,
        parent_ref.clone(),
        &stale_hash,
        30,
    ) {
        Ok(_) => panic!("stale plan must be rejected"),
        Err(rejection) => rejection,
    };
    assert_eq!(rejection.reason_code(), "plan_hash_stale");
    // Stale frontier is rejected.
    let mut stale_frontier = base.clone();
    stale_frontier.expected_durable_frontier = 0;
    let rejection = match crate::PlanExecutionService::approve(
        &mut session,
        parent_ref.clone(),
        &stale_frontier,
        30,
    ) {
        Ok(_) => panic!("stale frontier must be rejected"),
        Err(rejection) => rejection,
    };
    assert_eq!(rejection.reason_code(), "frontier_stale");
    // A Plan without its current review completion cannot be approved.
    let (mut not_ready, other_draft, _temp) = spine_session("plan_spine_3", "Not ready")?;
    let parent_ref = SessionRef::new_relative("parent.jsonl")?;
    let not_ready_command = sigil_kernel::PlanRunCommandV1 {
        command_id: "run-command-not-ready".to_owned(),
        session_id: not_ready.session_scope_id().to_owned(),
        plan_id: other_draft.plan_id.clone(),
        expected_plan_hash: other_draft.plan_hash.clone(),
        expected_durable_frontier: not_ready.durable_frontier_sequence(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreateAndRun,
        permission: sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy,
        source: sigil_kernel::PlanRunCommandSource::Http,
    };
    let rejection = match crate::PlanExecutionService::approve(
        &mut not_ready,
        parent_ref,
        &not_ready_command,
        30,
    ) {
        Ok(_) => panic!("unfinished review must be rejected"),
        Err(rejection) => rejection,
    };
    assert_eq!(rejection.reason_code(), "plan_not_ready");
    // The plan is not consumed by rejections.
    assert!(session.plan_artifact_projection().tasks_created.is_empty());
    Ok(())
}

#[test]
fn commit_draft_from_child_records_reviewable_text_without_execution_candidate() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session = Session::new("mock", "model");
    let mut session = session.with_store(JsonlSessionStore::new(temp.path().join("spine.jsonl"))?);
    let request = PlanReviewRunRequest {
        application_operation: None,
        plan_review_id: sigil_kernel::PlanReviewId::new("review-1")?,
        attempt_id: sigil_kernel::PlanReviewAttemptId::new("attempt-1")?,
        plan_id: sigil_kernel::PlanId::new("plan_spine_4")?,
        source: PlanReviewSource::ExplicitPlanCommand,
        source_turn: ConversationTurnRef {
            session_scope_id: session.session_scope_id().to_owned(),
            message_id: "message-1".to_owned(),
            logical_run_id: "run-1".to_owned(),
        },
        route_decision_id: None,
        child_session_ref: SessionRef::new_relative("child.jsonl")?,
        revision_request_id: None,
        revision_generation: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        explicit_objective: Some("implement".to_owned()),
        objective: "implement".to_owned(),
        workspace_snapshot_id: None,
    };
    let draft = draft_entry(&request);
    ensure_test_plan_review_attempt_started(&mut session, &request, 25)?;
    commit_test_plan_review_draft(&mut session, &draft, &request, 30)?;
    let artifacts = session.plan_artifact_projection();
    assert!(artifacts.plans.contains_key(&request.plan_id));
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
            if attempt.status == PlanReviewAttemptStatus::DraftReady
    )));
    // Retry is idempotent and does not append a duplicate draft.
    commit_test_plan_review_draft(&mut session, &draft, &request, 31)?;
    let artifacts = session.plan_artifact_projection();
    assert_eq!(artifacts.plans.len(), 1);
    Ok(())
}

#[test]
fn commit_draft_repeated_commit_is_idempotent_without_a_compile_contract() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session = Session::new("mock", "model");
    let mut session = session.with_store(JsonlSessionStore::new(temp.path().join("spine.jsonl"))?);
    let request = PlanReviewRunRequest {
        application_operation: None,
        plan_review_id: sigil_kernel::PlanReviewId::new("review-conflict")?,
        attempt_id: sigil_kernel::PlanReviewAttemptId::new("attempt-conflict")?,
        plan_id: sigil_kernel::PlanId::new("plan_spine_conflict")?,
        source: PlanReviewSource::ExplicitPlanCommand,
        source_turn: ConversationTurnRef {
            session_scope_id: session.session_scope_id().to_owned(),
            message_id: "message-1".to_owned(),
            logical_run_id: "run-1".to_owned(),
        },
        route_decision_id: None,
        child_session_ref: SessionRef::new_relative("child.jsonl")?,
        revision_request_id: None,
        revision_generation: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        explicit_objective: Some("implement".to_owned()),
        objective: "implement".to_owned(),
        workspace_snapshot_id: None,
    };
    let draft = draft_entry(&request);
    ensure_test_plan_review_attempt_started(&mut session, &request, 25)?;
    commit_test_plan_review_draft(&mut session, &draft, &request, 30)?;
    let committed_entries = session.entries().len();
    commit_test_plan_review_draft(&mut session, &draft, &request, 31)?;
    assert_eq!(session.entries().len(), committed_entries);
    // The durable draft remains unique.
    let artifacts = session.plan_artifact_projection();
    assert_eq!(artifacts.plans.len(), 1);
    Ok(())
}

#[test]
fn approve_rejects_commands_bound_to_another_session() -> Result<()> {
    let (mut session, draft, _temp) = spine_session("plan_spine_scope", "Session scope")?;
    mark_r69_plan_reviewable(&mut session, &draft)?;
    let parent_ref = SessionRef::new_relative("parent.jsonl")?;
    let command = sigil_kernel::PlanRunCommandV1 {
        command_id: "run-command-scope".to_owned(),
        session_id: "another-session".to_owned(),
        plan_id: draft.plan_id.clone(),
        expected_plan_hash: draft.plan_hash.clone(),
        expected_durable_frontier: session.durable_frontier_sequence(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreateAndRun,
        permission: sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy,
        source: sigil_kernel::PlanRunCommandSource::Http,
    };
    let rejection = crate::PlanExecutionService::approve(&mut session, parent_ref, &command, 30)
        .expect_err("cross-session command must be rejected");
    assert_eq!(rejection.reason_code(), "command_identity_conflict");
    assert!(session.plan_artifact_projection().tasks_created.is_empty());
    Ok(())
}

#[tokio::test]
async fn plan_review_raw_parent_source_survives_compaction_before_first_started() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent, request) = seed_route_decision_with_prior_context(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/parent.jsonl"),
        )?),
        true,
    )?;
    parent.append_assistant_message(ModelMessage::assistant(
        Some("source turn completed".to_owned()),
        Vec::new(),
    ))?;
    seed_completed_plan_input_history(&mut parent, "later parent")?;
    compact_plan_input_fixture(&parent, "parent")?;
    assert!(
        parent
            .try_context_projection_from_durable()?
            .expect("compacted parent")
            .model_messages()
            .iter()
            .all(|message| message.id != request.source_turn.message_id)
    );
    let provider = InvalidThenValidSubmissionProvider::default();
    let messages = provider.request_messages.clone();
    let agent = Agent::new(provider, ToolRegistry::new());
    let outcome = PlanReviewCoordinator::run_plan_review(
        &mut parent,
        &request,
        &agent,
        plan_review_test_options(temp.path()),
        ToolRegistry::new(),
        &mut NoopEventHandler,
        &mut AutoApproveHandler,
        RunCancellationOwner::new().handle(),
    )
    .await?;
    assert!(matches!(
        outcome,
        PlanReviewRunOutcome::CompletedWithoutDraft
    ));
    let messages = messages
        .lock()
        .map_err(|_| anyhow!("messages lock poisoned"))?;
    for message_set in messages.iter() {
        let objective = message_set
            .iter()
            .position(|message| message.content.as_deref() == Some(request.objective.as_str()))
            .context("objective")?;
        assert_eq!(message_set[objective - 2].id, "user-prior");
        assert_eq!(message_set[objective - 1].id, "assistant-prior");
        assert!(!message_set.iter().any(|message| {
            message
                .content
                .as_deref()
                .is_some_and(|text| text.contains("later parent"))
        }));
        assert_eq!(
            message_set
                .iter()
                .filter(|message| message.content.as_deref() == Some(request.objective.as_str()))
                .count(),
            1
        );
    }
    Ok(())
}

#[tokio::test]
async fn plan_review_preserves_full_candidate_and_compacted_research_without_finalizer()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let parent_path = temp.path().join("sessions/parent.jsonl");
    let (mut parent, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model")
            .with_store(JsonlSessionStore::new(&parent_path)?),
    )?;
    let child_path = request
        .child_session_ref
        .resolve(parent_path.parent().expect("parent directory"));
    let mut child = Session::new("plan-review-test", "planned-model")
        .with_store(JsonlSessionStore::new(&child_path)?);
    child.ensure_identity_entry()?;
    seed_completed_plan_input_history(&mut child, "retained research")?;
    compact_plan_input_fixture(&child, "research")?;
    drop(child);
    let full_research = format!(
        "research head\n{}\nresearch tail",
        "complete research evidence ".repeat(1500)
    );
    assert!(full_research.len() > 24 * 1024);
    let provider = InvalidThenValidSubmissionProvider {
        research_text: Some(full_research.clone()),
        ..Default::default()
    };
    let messages = provider.request_messages.clone();
    let agent = Agent::new(provider, ToolRegistry::new());
    let outcome = PlanReviewCoordinator::run_plan_review(
        &mut parent,
        &request,
        &agent,
        plan_review_test_options(temp.path()),
        ToolRegistry::new(),
        &mut NoopEventHandler,
        &mut AutoApproveHandler,
        RunCancellationOwner::new().handle(),
    )
    .await?;
    assert!(matches!(
        outcome,
        PlanReviewRunOutcome::CompletedWithoutDraft
    ));
    let messages = messages
        .lock()
        .map_err(|_| anyhow!("messages lock poisoned"))?;
    assert_eq!(messages.len(), 1);
    for message_set in messages.iter() {
        assert!(
            message_set.iter().any(|message| message
                .content
                .as_deref()
                .is_some_and(|text| text.contains("retained research"))),
            "the original research checkpoint remains available"
        );
        assert!(
            !message_set
                .iter()
                .any(|message| message.content.as_deref() == Some("retained research assistant 0")),
            "compaction must not resurrect folded raw messages"
        );
    }
    let candidate =
        parent
            .entries()
            .iter()
            .find_map(|entry| match entry {
                SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(
                    candidate,
                )) if candidate.content == full_research => Some(candidate),
                _ => None,
            })
            .context("the complete research answer must also be mirrored to the parent")?;
    assert_eq!(
        candidate.completeness,
        sigil_kernel::PlanReviewCandidateCompletenessV1::Unknown
    );
    let child = Session::load_from_store(
        "plan-review-test",
        "planned-model",
        JsonlSessionStore::new(&child_path)?,
    )?;
    assert!(!child.entries().iter().any(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft)) if draft.plan_id == request.plan_id)));
    assert!(child.entries().iter().any(|entry| matches!(entry,
        SessionLogEntry::Assistant(message) if message.content.as_deref() == Some(full_research.as_str()))));
    Ok(())
}

#[tokio::test]
async fn plan_review_input_rejects_missing_conflicting_and_cross_session_sources_before_provider()
-> Result<()> {
    struct UncommittedStarted;
    impl EventHandler for UncommittedStarted {
        fn handle(&mut self, _event: RunEvent) -> Result<()> {
            Ok(())
        }
        fn commit_controls(
            &mut self,
            _session: &mut Session,
            _controls: Vec<ControlEntry>,
        ) -> Result<Vec<sigil_kernel::StoredEvent>> {
            Ok(Vec::new())
        }
    }
    let temp = tempfile::tempdir()?;
    let (mut parent, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/parent.jsonl"),
        )?),
    )?;
    let provider = InvalidThenValidSubmissionProvider::default();
    let messages = provider.request_messages.clone();
    let agent = Agent::new(provider, ToolRegistry::new());
    let error = PlanReviewCoordinator::run_plan_review(
        &mut parent,
        &request,
        &agent,
        plan_review_test_options(temp.path()),
        ToolRegistry::new(),
        &mut UncommittedStarted,
        &mut AutoApproveHandler,
        RunCancellationOwner::new().handle(),
    )
    .await
    .expect_err("provider input requires an actually committed Started");
    assert_eq!(
        error.downcast_ref::<sigil_kernel::SessionContextPrefixError>(),
        Some(&sigil_kernel::SessionContextPrefixError::MissingBoundary)
    );
    for (source, expected) in [
        (
            "missing",
            sigil_kernel::SessionContextPrefixError::MissingSource,
        ),
        (
            "wrong-session",
            sigil_kernel::SessionContextPrefixError::ConflictingBoundary,
        ),
    ] {
        let mut invalid = request.clone();
        if source == "missing" {
            invalid.source_turn.message_id = "nonexistent-user".to_owned();
        } else {
            invalid.source_turn.session_scope_id = "another-parent".to_owned();
        }
        let before = parent.entries().len();
        let error = PlanReviewCoordinator::run_plan_review(
            &mut parent,
            &invalid,
            &agent,
            plan_review_test_options(temp.path()),
            ToolRegistry::new(),
            &mut NoopEventHandler,
            &mut AutoApproveHandler,
            RunCancellationOwner::new().handle(),
        )
        .await
        .expect_err("invalid source must be rejected");
        assert_eq!(
            error.downcast_ref::<sigil_kernel::SessionContextPrefixError>(),
            Some(&expected)
        );
        assert_eq!(parent.entries().len(), before);
    }
    let mut conflicting = parent
        .source_user_message(&request.source_turn.message_id)
        .expect("source")
        .clone();
    conflicting.content = Some("different objective under reused source id".to_owned());
    parent.append_user_message(conflicting)?;
    let error = PlanReviewCoordinator::run_plan_review(
        &mut parent,
        &request,
        &agent,
        plan_review_test_options(temp.path()),
        ToolRegistry::new(),
        &mut NoopEventHandler,
        &mut AutoApproveHandler,
        RunCancellationOwner::new().handle(),
    )
    .await
    .expect_err("conflicting raw source must be rejected");
    assert_eq!(
        error.downcast_ref::<sigil_kernel::SessionContextPrefixError>(),
        Some(&sigil_kernel::SessionContextPrefixError::ConflictingSource)
    );
    assert!(
        messages
            .lock()
            .map_err(|_| anyhow!("messages lock poisoned"))?
            .is_empty()
    );
    let mut wrong_binding = request.clone();
    wrong_binding.child_session_ref = SessionRef::new_relative("another-child.jsonl")?;
    let error = PlanReviewCoordinator::ensure_attempt_started(
        &mut parent,
        &wrong_binding,
        &mut NoopEventHandler,
        999,
    )
    .expect_err("first attempt binding cannot drift");
    assert_eq!(
        error.downcast_ref::<sigil_kernel::SessionContextPrefixError>(),
        Some(&sigil_kernel::SessionContextPrefixError::ConflictingBoundary)
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_research_preserves_early_tools_beyond_twelve_results() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/parent.jsonl"),
        )?),
    )?;
    let provider = LoopingPlanReviewProvider {
        research_turns: Some(13),
        ..Default::default()
    };
    let messages = Arc::clone(&provider.request_messages);
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(PlanReviewInspectionTool));
    let outcome = PlanReviewCoordinator::run_plan_review(
        &mut parent,
        &request,
        &Agent::new(provider, ToolRegistry::new()),
        plan_review_test_options(temp.path()),
        tools,
        &mut NoopEventHandler,
        &mut AutoApproveHandler,
        RunCancellationOwner::new().handle(),
    )
    .await?;
    assert!(matches!(
        outcome,
        PlanReviewRunOutcome::CompletedWithoutDraft
    ));
    let messages = messages
        .lock()
        .map_err(|_| anyhow!("messages lock poisoned"))?;
    assert_eq!(messages.len(), 14);
    let next_request = messages.last().context("next model request missing")?;
    assert!(
        next_request
            .iter()
            .any(|message| message.tool_calls.iter().any(|call| call.id == "inspect-0")),
        "the earliest research tool must survive in the ordinary model loop"
    );
    assert!(
        next_request
            .iter()
            .any(|message| message.tool_call_id.as_deref() == Some("inspect-0")),
        "the earliest paired result must survive in the ordinary model loop"
    );
    assert!(parent.entries().iter().any(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(candidate))
            if candidate.content == "research did not converge")));

    Ok(())
}

#[tokio::test]
async fn plan_review_long_typed_no_plan_does_not_depend_on_truncated_preview() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/parent.jsonl"),
        )?),
    )?;
    let reason = format!(
        "No change is justified.\n{}\nNo remaining migration is warranted.",
        "The current evidence already satisfies the request. ".repeat(800)
    );
    let provider = NoPlanPlanReviewProvider {
        reason: Some(reason.clone()),
        ..Default::default()
    };
    let calls = Arc::clone(&provider.request_tools);
    let outcome = PlanReviewCoordinator::run_plan_review(
        &mut parent,
        &request,
        &Agent::new(provider, ToolRegistry::new()),
        plan_review_test_options(temp.path()),
        ToolRegistry::new(),
        &mut NoopEventHandler,
        &mut AutoApproveHandler,
        RunCancellationOwner::new().handle(),
    )
    .await?;
    assert!(matches!(
        outcome,
        PlanReviewRunOutcome::CompletedWithoutDraft
    ));
    assert_eq!(
        calls
            .lock()
            .map_err(|_| anyhow!("request lock poisoned"))?
            .len(),
        1
    );
    assert!(parent.plan_artifact_projection().plans.is_empty());
    assert!(parent.entries().iter().any(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(candidate))
            if candidate.content == reason)));
    let child = Session::load_from_store(
        "plan-review-test",
        "planned-model",
        JsonlSessionStore::new(
            request
                .child_session_ref
                .resolve(temp.path().join("sessions").as_path()),
        )?,
    )?;
    let result = child
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::ToolResultV3(result) if result.call_id == "no-plan" => Some(result),
            _ => None,
        })
        .context("successful no_plan receipt missing")?;
    assert_eq!(result.facts.status, "ok");
    assert!(
        sigil_kernel::decode_plan_review_result(&result.initial_model_view.preview).is_err(),
        "this fixture must cross the tool preview bound"
    );
    Ok(())
}

#[tokio::test]
async fn r71_f_csr_005_model_submitted_draft_uses_the_admitted_research_bundle() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;
    let (temp, provisioner, _) = child_resource_fixture(&[
        Channel::SessionLog,
        Channel::ArtifactStaging,
        Channel::ArtifactStore,
    ])?;
    let (mut parent, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model")
            .with_store(JsonlSessionStore::new(temp.path().join("parent.jsonl"))?),
    )?;
    let outcome = PlanReviewCoordinator::run_plan_review_with_resource_provisioner(
        &mut parent,
        &request,
        &Agent::new(ResearchThenDraftProvider::default(), ToolRegistry::new()),
        plan_review_test_options(temp.path()),
        ToolRegistry::new(),
        &mut NoopEventHandler,
        &mut AutoApproveHandler,
        RunCancellationOwner::new().handle(),
        Arc::clone(&provisioner),
    )
    .await?;
    assert!(matches!(outcome, PlanReviewRunOutcome::DraftReady { .. }));
    let bundle = provisioner.provision(&request)?;
    let entries = JsonlSessionStore::read_entries(bundle.session_log_path())?;
    assert!(entries.iter().any(|entry| matches!(entry,
        SessionLogEntry::Assistant(message) if message.content.as_deref() == Some("research complete"))));
    assert!(entries.iter().any(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft)) if draft.plan_id == request.plan_id)),
        "the confirmed draft belongs to the same research child");
    assert!(
        entries.iter().any(|entry| matches!(entry,
        SessionLogEntry::ToolResultV3(result)
            if result.tool_name == sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME
                && result.facts.status == "ok" && result.artifact.descriptor().is_some())),
        "typed finalization uses the original managed artifact backend"
    );
    bundle.finish()?;
    Ok(())
}

async fn managed_plan_review_uncommitted_draft_fixture(
    revision: bool,
) -> Result<(
    tempfile::TempDir,
    Session,
    PlanReviewRunRequest,
    PlanDraftCreatedEntry,
    Arc<dyn crate::plan_review_coordinator::PlanReviewChildResourceProvisionerV1>,
    std::path::PathBuf,
)> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;
    let (temp, provisioner, _) = child_resource_fixture(&[
        Channel::SessionLog,
        Channel::ArtifactStaging,
        Channel::ArtifactStore,
    ])?;
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let parent = Session::load_from_store("plan-review-test", "planned-model", parent_store)?;
    let (mut parent, mut request) = seed_route_decision(parent)?;
    if revision {
        ensure_test_plan_review_attempt_started(&mut parent, &request, 10)?;
        let base = draft_entry(&request);
        commit_test_plan_review_draft(&mut parent, &base, &request, 11)?;
        request = submit_revision_guidance(&mut parent, &base)?;
        // Application dispatch records revision Started before the first managed admission.
        ensure_test_plan_review_attempt_started(&mut parent, &request, 22)?;
    }
    let PlanReviewRunOutcome::DraftReady { draft } =
        PlanReviewCoordinator::run_plan_review_with_resource_provisioner(
            &mut parent,
            &request,
            &Agent::new(ResearchThenDraftProvider::default(), ToolRegistry::new()),
            plan_review_test_options(temp.path()),
            ToolRegistry::new(),
            &mut NoopEventHandler,
            &mut AutoApproveHandler,
            RunCancellationOwner::new().handle(),
            Arc::clone(&provisioner),
        )
        .await?
    else {
        bail!("fixture must durably submit a typed draft");
    };
    assert!(
        !parent
            .plan_artifact_projection()
            .plans
            .contains_key(&request.plan_id),
        "fixture stops after child completion and before its parent commit"
    );
    assert_eq!(
        PlanReviewProjection::from_entries(parent.entries())
            .latest_attempt(&request.plan_review_id)
            .map(|attempt| attempt.status),
        Some(PlanReviewAttemptStatus::Started)
    );
    let bundle = provisioner.provision(&request)?;
    let child_path = bundle.session_log_path().to_path_buf();
    bundle.finish()?;
    assert_ne!(child_path, request.child_session_ref.resolve(temp.path()));
    assert!(!request.child_session_ref.resolve(temp.path()).exists());
    Ok((temp, parent, request, *draft, provisioner, child_path))
}

#[tokio::test]
async fn managed_plan_review_recovers_submitted_draft_without_redispatch() -> Result<()> {
    let (temp, parent, request, draft, provisioner, child_path) =
        managed_plan_review_uncommitted_draft_fixture(false).await?;
    let parent_path = parent.store_path().context("durable parent")?.to_path_buf();
    let child_bytes = std::fs::read(&child_path)?;
    drop(parent);
    let mut parent = Session::load_from_store_for_control(JsonlSessionStore::new(&parent_path)?)?;
    let parent_before_recovery = std::fs::read(&parent_path)?;
    let provider = PlainTextOnlyReviewProvider::default();
    let calls = Arc::clone(&provider.request_tools);
    let agent = Agent::new(provider, ToolRegistry::new());
    for _ in 0..2 {
        let PlanReviewRunOutcome::DraftReady { draft: recovered } =
            PlanReviewCoordinator::run_plan_review_with_resource_provisioner(
                &mut parent,
                &request,
                &agent,
                plan_review_test_options(temp.path()),
                ToolRegistry::new(),
                &mut NoopEventHandler,
                &mut AutoApproveHandler,
                RunCancellationOwner::new().handle(),
                Arc::clone(&provisioner),
            )
            .await?
        else {
            bail!("the original typed draft must recover without another run");
        };
        assert_eq!(*recovered, draft);
    }
    assert!(
        calls
            .lock()
            .map_err(|_| anyhow!("calls lock poisoned"))?
            .is_empty()
    );
    assert_eq!(std::fs::read(&child_path)?, child_bytes);
    assert_eq!(std::fs::read(&parent_path)?, parent_before_recovery);
    drop(parent);

    assert_eq!(
        PlanReviewCoordinator::recover_managed_plan_review_drafts_from_store(
            JsonlSessionStore::new(&parent_path)?,
            provisioner.as_ref(),
            100,
        )?,
        1
    );
    let parent = Session::load_from_store(
        "plan-review-test",
        "planned-model",
        JsonlSessionStore::new(&parent_path)?,
    )?;
    assert_eq!(
        parent
            .plan_artifact_projection()
            .plans
            .get(&request.plan_id),
        Some(&draft)
    );
    assert_eq!(
        PlanReviewProjection::from_entries(parent.entries())
            .latest_attempt(&request.plan_review_id)
            .map(|attempt| attempt.status),
        Some(PlanReviewAttemptStatus::DraftReady)
    );
    let committed_bytes = std::fs::read(&parent_path)?;
    assert_eq!(
        PlanReviewCoordinator::recover_managed_plan_review_drafts_from_store(
            JsonlSessionStore::new(&parent_path)?,
            provisioner.as_ref(),
            101,
        )?,
        0
    );
    assert_eq!(std::fs::read(&parent_path)?, committed_bytes);
    assert_eq!(std::fs::read(&child_path)?, child_bytes);
    Ok(())
}

#[tokio::test]
async fn managed_plan_review_revision_recovery_commits_exact_terminal_outbox() -> Result<()> {
    let (_temp, parent, request, draft, provisioner, child_path) =
        managed_plan_review_uncommitted_draft_fixture(true).await?;
    let parent_path = parent.store_path().context("durable parent")?.to_path_buf();
    let child_bytes = std::fs::read(&child_path)?;
    drop(parent);
    assert_eq!(
        PlanReviewCoordinator::recover_managed_plan_review_drafts_from_store(
            JsonlSessionStore::new(&parent_path)?,
            provisioner.as_ref(),
            100,
        )?,
        1
    );
    let mut parent = Session::load_from_store(
        "plan-review-test",
        "planned-model",
        JsonlSessionStore::new(&parent_path)?,
    )?;
    let outbox = parent
        .reconcile_plan_review_revision_terminal(&request.child_logical_run_id())?
        .context("recovered revision must own its exact terminal outbox")?;
    let PlanReviewRunOutcome::DraftReady { draft: recovered } =
        PlanReviewCoordinator::revision_outcome_from_terminal(&parent, &request, &outbox)?
    else {
        bail!("recovered revision must retain its exact draft");
    };
    assert_eq!(*recovered, draft);
    assert_eq!(
        parent
            .plan_artifact_projection()
            .latest_decision(request.base_plan_id.as_ref().context("revision base")?)
            .context("revision decision")?
            .decision,
        PlanDecision::RevisionSucceeded
    );
    assert_eq!(
        PlanReviewCoordinator::recover_managed_plan_review_drafts(
            &mut parent,
            provisioner.as_ref(),
            101,
        )?,
        0
    );
    assert_eq!(std::fs::read(&child_path)?, child_bytes);
    Ok(())
}

#[tokio::test]
async fn managed_plan_review_recovery_keeps_cancelled_parent_terminal() -> Result<()> {
    let (_temp, mut parent, request, _draft, provisioner, child_path) =
        managed_plan_review_uncommitted_draft_fixture(false).await?;
    PlanReviewCoordinator::close_plan_review_run(
        &mut parent,
        &request,
        &PlanReviewRunOutcome::Cancelled,
        &mut NoopEventHandler,
        100,
    )?;
    let parent_path = parent.store_path().context("durable parent")?.to_path_buf();
    let parent_bytes = std::fs::read(&parent_path)?;
    let child_bytes = std::fs::read(&child_path)?;
    assert!(
        PlanReviewCoordinator::recover_managed_plan_review_draft(
            &parent,
            &request,
            provisioner.as_ref(),
        )?
        .is_none()
    );
    assert_eq!(
        PlanReviewCoordinator::recover_managed_plan_review_drafts_from_store(
            JsonlSessionStore::new(&parent_path)?,
            provisioner.as_ref(),
            101,
        )?,
        0
    );
    assert!(
        !parent
            .plan_artifact_projection()
            .plans
            .contains_key(&request.plan_id)
    );
    assert_eq!(std::fs::read(parent_path)?, parent_bytes);
    assert_eq!(std::fs::read(child_path)?, child_bytes);
    Ok(())
}

#[test]
fn managed_plan_review_missing_namespace_does_not_block_or_initialize_startup() -> Result<()> {
    use crate::managed_storage_writer::{
        ManagedStorageWriterErrorV1, StorageWriterChannelV1 as Channel,
    };
    let (temp, provisioner, _) = child_resource_fixture(&[
        Channel::SessionLog,
        Channel::ArtifactStaging,
        Channel::ArtifactStore,
    ])?;
    let parent_path = temp.path().join("parent.jsonl");
    let parent = Session::load_from_store(
        "plan-review-test",
        "planned-model",
        JsonlSessionStore::new(&parent_path)?,
    )?;
    let (mut parent, request) = seed_route_decision(parent)?;
    ensure_test_plan_review_attempt_started(&mut parent, &request, 10)?;
    let parent_bytes = std::fs::read(&parent_path)?;
    for _ in 0..2 {
        let error = provisioner
            .recover_research_session(&request)
            .expect_err("required-child recovery must retain typed namespace absence");
        assert_eq!(
            error.downcast_ref::<ManagedStorageWriterErrorV1>(),
            Some(&ManagedStorageWriterErrorV1::ExistingNamespaceMissing)
        );
        assert_eq!(
            PlanReviewCoordinator::recover_managed_plan_review_drafts_from_store(
                JsonlSessionStore::new(&parent_path)?,
                provisioner.as_ref(),
                20,
            )?,
            0
        );
        assert_eq!(std::fs::read(&parent_path)?, parent_bytes);
        assert!(!request.child_session_ref.resolve(temp.path()).exists());
    }
    drop(parent);
    let parent = Session::load_from_store(
        "plan-review-test",
        "planned-model",
        JsonlSessionStore::new(&parent_path)?,
    )?;
    assert_eq!(
        PlanReviewProjection::from_entries(parent.entries())
            .latest_attempt(&request.plan_review_id)
            .map(|attempt| attempt.status),
        Some(PlanReviewAttemptStatus::Interrupted),
        "generic startup retains its existing interrupted-attempt recovery"
    );
    Ok(())
}

#[test]
fn managed_plan_review_uninitialized_child_keeps_startup_and_retry_available() -> Result<()> {
    use crate::managed_storage_writer::{
        ManagedStorageWriterErrorV1, StorageWriterChannelV1 as Channel,
    };
    for empty_record in [false, true] {
        let (temp, provisioner, _) = child_resource_fixture(&[
            Channel::SessionLog,
            Channel::ArtifactStaging,
            Channel::ArtifactStore,
        ])?;
        let parent_path = temp.path().join("parent.jsonl");
        let parent = Session::load_from_store(
            "plan-review-test",
            "planned-model",
            JsonlSessionStore::new(&parent_path)?,
        )?;
        let (mut parent, request) = seed_route_decision(parent)?;
        let bundle = provisioner.provision(&request)?;
        let child_path = bundle.session_log_path().to_path_buf();
        bundle.finish()?;
        ensure_test_plan_review_attempt_started(&mut parent, &request, 10)?;
        assert!(!child_path.exists());
        if empty_record {
            std::fs::File::create(&child_path)?;
            sigil_kernel::secure_private_path_permissions(&child_path)?;
        }
        let parent_bytes = std::fs::read(&parent_path)?;
        let error = provisioner
            .recover_research_session(&request)
            .expect_err("required-child read must reject an unpublished stream");
        assert_eq!(
            error.downcast_ref::<ManagedStorageWriterErrorV1>(),
            Some(&ManagedStorageWriterErrorV1::ExistingSessionLogUninitialized)
        );
        assert_eq!(
            PlanReviewCoordinator::recover_managed_plan_review_drafts_from_store(
                JsonlSessionStore::new(&parent_path)?,
                provisioner.as_ref(),
                20,
            )?,
            0
        );
        assert_eq!(std::fs::read(&parent_path)?, parent_bytes);
        assert_eq!(child_path.exists(), empty_record);
        if empty_record {
            assert!(std::fs::read(&child_path)?.is_empty());
        }
        drop(parent);
        let parent = Session::load_from_store(
            "plan-review-test",
            "planned-model",
            JsonlSessionStore::new(&parent_path)?,
        )?;
        assert_eq!(
            PlanReviewProjection::from_entries(parent.entries())
                .latest_attempt(&request.plan_review_id)
                .map(|attempt| attempt.status),
            Some(PlanReviewAttemptStatus::Interrupted)
        );
        assert!(
            public_plan_review_from_session(&parent)?
                .allowed_actions
                .contains(&sigil_kernel::PublicPlanAction::RetryReview)
        );
    }
    Ok(())
}

#[test]
fn automatic_review_context_includes_current_progress_and_freezes_at_route_receipt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let (mut session, request) = seed_route_decision_with_progress(
        Session::load_from_store("plan-review-test", "planned-model", store.clone())?,
        true,
        true,
    )?;
    session.append_user_message(ModelMessage::user("later request before review dispatch"))?;
    ensure_test_plan_review_attempt_started(&mut session, &request, 50)?;
    let original = crate::plan_review_coordinator::plan_review_parent_context(&session, &request)?;
    assert!(!original.iter().any(|message| {
        message
            .tool_calls
            .iter()
            .any(|call| call.id == "review-handoff")
            || message.tool_call_id.as_deref() == Some("review-handoff")
    }));
    assert!(original.iter().any(|message| message.content.as_deref()
        == Some("already inspected migration dependencies before planning")));
    assert!(
        !original
            .iter()
            .any(|message| message.content.as_deref()
                == Some("later request before review dispatch"))
    );
    let draft_context = sigil_kernel::PlanReviewDraftContext {
        plan_review_id: request.plan_review_id.clone(),
        attempt_id: request.attempt_id.clone(),
        plan_id: request.plan_id.clone(),
        source: request.plan_source_ref(),
        workspace_snapshot_id: request.workspace_snapshot_id.clone(),
    };
    let cancellation = RunCancellationOwner::new();
    let first_input = crate::plan_review_coordinator::plan_review_run_input(
        &request,
        &draft_context,
        &cancellation.handle(),
        &original,
    );
    drop(session);
    let reloaded = Session::load_from_store("plan-review-test", "planned-model", store)?;
    let reopened_context =
        crate::plan_review_coordinator::plan_review_parent_context(&reloaded, &request)?;
    let recovery_input = crate::plan_review_coordinator::plan_review_run_input(
        &request,
        &draft_context,
        &cancellation.handle(),
        &reopened_context,
    );
    assert_eq!(
        serde_json::to_value(&first_input.initial_context)?,
        serde_json::to_value(&recovery_input.initial_context)?
    );
    assert_eq!(
        serde_json::to_value(&first_input.transient_context)?,
        serde_json::to_value(&recovery_input.transient_context)?
    );
    let again = crate::plan_review_coordinator::plan_review_run_input(
        &request,
        &draft_context,
        &cancellation.handle(),
        &[],
    );
    assert_eq!(
        serde_json::to_value(&first_input.transient_context)?,
        serde_json::to_value(&again.transient_context)?
    );
    assert_eq!(
        serde_json::to_value(original)?,
        serde_json::to_value(crate::plan_review_coordinator::plan_review_parent_context(
            &reloaded, &request
        )?)?
    );
    Ok(())
}

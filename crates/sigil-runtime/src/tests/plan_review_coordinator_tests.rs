use std::{
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
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
    ConversationTurnRef, EventHandler, InteractionMode, JsonlSessionStore, MemoryConfig,
    ModelMessage, NoopEventHandler, PermissionConfig, PermissionEvaluationContext, PlanDecision,
    PlanDraftCreatedEntry, PlanReviewAttemptStatus, PlanReviewProjection, PlanReviewSource,
    Provider, ProviderCapabilities, ProviderChunk, PublicRunEventKind, RunCancellationOwner,
    RunEvent, Session, SessionLogEntry, SessionRef, TaskRoutingPolicy, TaskRunStatus, Tool,
    ToolAccess, ToolCall, ToolCategory, ToolContext, ToolPreviewCapability, ToolRegistry,
    ToolResult, ToolResultMeta, ToolSpec, conversation_route_decision_id_for_source,
    plan_review_attempt_id_for_review, plan_review_plan_id_for_attempt,
    plan_review_policy_snapshot_hash,
};

use crate::PlanReviewRunOutcome;
use crate::{
    ConversationCoordinator, PlanDecisionCommand, PlanReviewCoordinator, PlanReviewRunRequest,
};

fn isolated_storage_toml(path: &std::path::Path) -> String {
    let root = path.parent().expect("test config should have a parent");
    let state_root = toml::Value::String(root.join("state").to_string_lossy().into_owned());
    let cache_root = toml::Value::String(root.join("cache").to_string_lossy().into_owned());
    format!("[storage]\nstate_root = {state_root}\ncache_root = {cache_root}\n")
}

#[test]
fn current_schema_child_resource_bundle_is_scoped_and_explicitly_finalized() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;
    use crate::plan_review_coordinator::PlanReviewChildResourceKindV1;

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

    let bundle = provisioner.provision(&request, PlanReviewChildResourceKindV1::Research, 0)?;
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
    let recovered = provisioner.provision(&request, PlanReviewChildResourceKindV1::Research, 0)?;
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
    use crate::plan_review_coordinator::PlanReviewChildResourceKindV1;
    let (_temp, provisioner, request) = child_resource_fixture(&[
        Channel::SessionLog,
        Channel::ArtifactStaging,
        Channel::ArtifactStore,
    ])?;
    let bundle = provisioner.provision(&request, PlanReviewChildResourceKindV1::Research, 0)?;
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
    use crate::plan_review_coordinator::PlanReviewChildResourceKindV1;
    let (_temp, provisioner, request) = child_resource_fixture(&[
        Channel::SessionLog,
        Channel::ArtifactStaging,
        Channel::ArtifactStore,
    ])?;
    let bundle = provisioner.provision(&request, PlanReviewChildResourceKindV1::Research, 0)?;
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
    use crate::plan_review_coordinator::PlanReviewChildResourceKindV1;
    let (temp, provisioner, request) = child_resource_fixture(&[
        Channel::SessionLog,
        Channel::ArtifactStaging,
        Channel::ArtifactStore,
    ])?;
    let bundle = provisioner.provision(&request, PlanReviewChildResourceKindV1::Research, 0)?;
    let debug = format!("{bundle:?}");
    assert!(!debug.contains(temp.path().to_string_lossy().as_ref()));
    assert!(!debug.contains("records.jsonl"));
    bundle.finish()?;
    Ok(())
}

#[test]
fn r71_f_csr_004_research_finish_releases_the_exact_admissions() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;
    use crate::plan_review_coordinator::PlanReviewChildResourceKindV1;
    let (_temp, provisioner, request) = child_resource_fixture(&[
        Channel::SessionLog,
        Channel::ArtifactStaging,
        Channel::ArtifactStore,
    ])?;
    let bundle = provisioner.provision(&request, PlanReviewChildResourceKindV1::Research, 0)?;
    bundle.finish()?;
    let recovered = provisioner.provision(&request, PlanReviewChildResourceKindV1::Research, 0)?;
    recovered.finish()?;
    Ok(())
}

#[test]
fn r71_f_csr_005_finalizer_finish_releases_the_exact_admissions() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;
    use crate::plan_review_coordinator::PlanReviewChildResourceKindV1;
    let (_temp, provisioner, request) = child_resource_fixture(&[
        Channel::SessionLog,
        Channel::ArtifactStaging,
        Channel::ArtifactStore,
    ])?;
    let bundle = provisioner.provision(&request, PlanReviewChildResourceKindV1::Finalizer, 1)?;
    bundle.finish()?;
    let recovered = provisioner.provision(&request, PlanReviewChildResourceKindV1::Finalizer, 1)?;
    recovered.finish()?;
    Ok(())
}

#[test]
fn r71_f_csr_006_research_and_finalizer_never_share_scope() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;
    use crate::plan_review_coordinator::PlanReviewChildResourceKindV1;
    let (_temp, provisioner, request) = child_resource_fixture(&[
        Channel::SessionLog,
        Channel::ArtifactStaging,
        Channel::ArtifactStore,
    ])?;
    let research = provisioner.provision(&request, PlanReviewChildResourceKindV1::Research, 0)?;
    let finalizer = provisioner.provision(&request, PlanReviewChildResourceKindV1::Finalizer, 1)?;
    assert_ne!(research.scope_id(), finalizer.scope_id());
    assert!(research.scope_id().ends_with("-research"));
    assert!(finalizer.scope_id().ends_with("-finalizer"));
    finalizer.finish()?;
    research.finish()?;
    Ok(())
}

#[test]
fn r71_f_csr_007_missing_artifact_authority_fails_before_child_bundle() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;
    use crate::plan_review_coordinator::PlanReviewChildResourceKindV1;
    let (_temp, provisioner, request) = child_resource_fixture(&[Channel::SessionLog])?;
    let error = provisioner
        .provision(&request, PlanReviewChildResourceKindV1::Research, 0)
        .expect_err("missing artifact authority");
    assert!(error.to_string().contains("artifact-staging"));
    Ok(())
}

#[test]
fn managed_child_partial_artifact_admission_settles_session_log() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;
    use crate::plan_review_coordinator::PlanReviewChildResourceKindV1;

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
        .provision(&request, PlanReviewChildResourceKindV1::Research, 0)
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
fn r71_f_csr_008_child_scope_is_retry_stable_and_kind_bound() -> Result<()> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Channel;
    use crate::plan_review_coordinator::PlanReviewChildResourceKindV1;
    let (_temp, provisioner, request) = child_resource_fixture(&[
        Channel::SessionLog,
        Channel::ArtifactStaging,
        Channel::ArtifactStore,
    ])?;
    let first = provisioner.provision(&request, PlanReviewChildResourceKindV1::Finalizer, 1)?;
    let first_scope = first.scope_id().to_owned();
    first.finish()?;
    let retry = provisioner.provision(&request, PlanReviewChildResourceKindV1::Finalizer, 1)?;
    assert_eq!(retry.scope_id(), first_scope);
    retry.finish()?;
    let next = provisioner.provision(&request, PlanReviewChildResourceKindV1::Finalizer, 2)?;
    assert_eq!(next.scope_id(), first_scope);
    next.finish()?;
    Ok(())
}

fn session_with_route_decision() -> Result<(Session, PlanReviewRunRequest)> {
    seed_route_decision(Session::new("plan-review-test", "planned-model"))
}

fn seed_route_decision(session: Session) -> Result<(Session, PlanReviewRunRequest)> {
    seed_route_decision_with_prior_context(session, false)
}

fn seed_route_decision_with_prior_context(
    mut session: Session,
    include_prior_context: bool,
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
    compile_input: &sigil_kernel::PlanCompileInputV1,
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
        compile_input,
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

fn candidate_confirmation_chunks(call_id: &str, valid: bool) -> Vec<Result<ProviderChunk>> {
    let args = if valid {
        r#"{"decision":"accept"}"#
    } else {
        r#"{"decision":"accept","content":"candidate replacement is forbidden"}"#
    };
    vec![
        Ok(ProviderChunk::ToolCallComplete(ToolCall {
            id: call_id.to_owned(),
            name: sigil_kernel::CONFIRM_PLAN_REVIEW_CANDIDATE_TOOL_NAME.to_owned(),
            args_json: args.to_owned(),
        })),
        Ok(ProviderChunk::Done),
    ]
}

fn no_plan_chunks(call_id: &str) -> Vec<Result<ProviderChunk>> {
    let args =
        r#"{"schema_version":1,"outcome":"no_plan","content":"No safe migration is warranted."}"#;
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
    request_messages: Arc<Mutex<Vec<Vec<ModelMessage>>>>,
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
struct ViolatingFinalizerProvider {
    request_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[derive(Clone, Default)]
struct InvalidThenValidFinalizerProvider {
    request_tools: Arc<Mutex<Vec<Vec<String>>>>,
    request_messages: Arc<Mutex<Vec<Vec<ModelMessage>>>>,
    interrupt_research: bool,
}

#[derive(Clone, Default)]
struct NoPlanPlanReviewProvider {
    request_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[derive(Clone, Default)]
struct PlainTextFinalizerProvider {
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
        let chunks = if call == 0 {
            vec![
                Ok(ProviderChunk::TextDelta("research complete".to_owned())),
                Ok(ProviderChunk::Done),
            ]
        } else {
            submitted_draft_chunks("finalizer-draft")
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}

#[async_trait]
impl Provider for PlainTextFinalizerProvider {
    fn name(&self) -> &str {
        "plain-text-plan-finalizer"
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
        self.request_messages
            .lock()
            .map_err(|_| anyhow!("plan review message recorder lock poisoned"))?
            .push(request.messages.clone());
        let confirms_candidate = request
            .tools
            .iter()
            .any(|tool| tool.name == sigil_kernel::CONFIRM_PLAN_REVIEW_CANDIDATE_TOOL_NAME);
        Ok(Box::pin(stream::iter(match index {
            0 if self.interrupt_research => vec![
                Ok(ProviderChunk::TextDelta(
                    "partial research response".to_owned(),
                )),
                Err(anyhow!("simulated TLS unexpected EOF")),
            ],
            0 => vec![
                Ok(ProviderChunk::TextDelta("research complete".to_owned())),
                Ok(ProviderChunk::Done),
            ],
            1 if confirms_candidate => candidate_confirmation_chunks("invalid-confirmation", false),
            _ if confirms_candidate => {
                candidate_confirmation_chunks("corrected-confirmation", true)
            }
            1 => invalid_draft_chunks("invalid-draft"),
            _ => submitted_draft_chunks("corrected-draft"),
        })))
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
        Ok(Box::pin(stream::iter(no_plan_chunks("no-plan"))))
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
    commit_test_plan_review_draft(
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
    use crate::plan_review_coordinator::PlanReviewChildResourceKindV1;

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

    let bundle = provisioner.provision(&request, PlanReviewChildResourceKindV1::Research, 0)?;
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
    use crate::plan_review_coordinator::PlanReviewChildResourceKindV1;

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
    let bundle = provisioner.provision(&request, PlanReviewChildResourceKindV1::Research, 0)?;
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
    use crate::plan_review_coordinator::PlanReviewChildResourceKindV1;

    let (_temp, parent, request, pending, provisioner) =
        managed_plan_review_research_waiting_fixture().await?;
    let bundle = provisioner.provision(&request, PlanReviewChildResourceKindV1::Research, 0)?;
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
    use crate::plan_review_coordinator::PlanReviewChildResourceKindV1;

    let (_temp, parent, request, pending, provisioner) =
        managed_plan_review_research_waiting_fixture().await?;
    let bundle = provisioner.provision(&request, PlanReviewChildResourceKindV1::Research, 0)?;
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
        "a declined clarification resumes the same attempt's submit-only finalization"
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
    use crate::plan_review_coordinator::PlanReviewChildResourceKindV1;

    let (_temp, parent, request, _pending, provisioner) =
        managed_plan_review_research_waiting_fixture().await?;
    let bundle = provisioner.provision(&request, PlanReviewChildResourceKindV1::Research, 0)?;
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
    use crate::plan_review_coordinator::PlanReviewChildResourceKindV1;

    let (_temp, mut parent, request, pending, provisioner) =
        managed_plan_review_research_waiting_fixture().await?;
    let bundle = provisioner.provision(&request, PlanReviewChildResourceKindV1::Research, 0)?;
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
async fn plan_review_research_uses_the_configured_budget_before_finalization() -> Result<()> {
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
        10,
        "research may continue past the legacy four-turn bound before one finalization turn"
    );
    assert!(requests[..9].iter().all(|tools| {
        tools.iter().any(|name| name == "inspect_workspace")
            && tools
                .iter()
                .any(|name| name == sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME)
    }));
    assert_eq!(
        requests[9],
        vec![sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned()],
        "the finalization turn must not retain research or hosted-tool preparation"
    );
    let messages = request_messages
        .lock()
        .map_err(|_| anyhow!("plan review message recorder lock poisoned"))?;
    let checkpoint_counts = messages
        .iter()
        .map(|request| {
            request
                .iter()
                .filter_map(|message| message.content.as_deref())
                .filter(|content| content.contains("Reassess the evidence gathered so far"))
                .count()
        })
        .collect::<Vec<_>>();
    assert!(
        checkpoint_counts.contains(&1),
        "the eighth research turn receives an advisory checkpoint"
    );
    assert!(
        checkpoint_counts.iter().all(|count| *count <= 1),
        "the checkpoint is injected at most once into each provider request"
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_typed_no_plan_closes_without_starting_finalizer() -> Result<()> {
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
        "no_plan must not start a submit-only finalizer"
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
    assert!(matches!(outcome, PlanReviewRunOutcome::DraftReady { .. }));

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
    assert!(
        requests.len() >= 4,
        "must exercise multiple research turns and finalization"
    );
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
async fn plan_revision_submit_only_non_submit_is_never_dispatched_and_closes_typed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent_session, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/session.jsonl"),
        )?),
    )?;
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
            .all(|tools| { tools == &[sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned()] })
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_invalid_typed_finalizer_retries_once_in_a_fresh_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent_session, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/session.jsonl"),
        )?),
    )?;
    let provider = InvalidThenValidFinalizerProvider::default();
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

    assert!(matches!(outcome, PlanReviewRunOutcome::DraftReady { .. }));
    let requests = request_tools
        .lock()
        .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
    assert_eq!(requests.len(), 3, "research plus two submit-only attempts");
    assert!(
        requests[1..]
            .iter()
            .all(|tools| tools == &[sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned()])
    );
    assert!(handler.0.iter().any(|event| matches!(
        event,
        RunEvent::Notice(message) if message.contains("invalid typed draft")
    )));
    let messages = request_messages
        .lock()
        .map_err(|_| anyhow!("plan review message recorder lock poisoned"))?;
    let retry_text = messages
        .get(2)
        .context("corrective finalizer request was not recorded")?
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>();
    assert!(
        retry_text
            .iter()
            .any(|text| text.contains("Previous submit-only attempt was rejected")),
        "corrective finalizer must receive the validation feedback"
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_plain_text_finalizer_is_preserved_as_a_candidate_until_explicit_adoption()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent_session, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/session.jsonl"),
        )?),
    )?;
    let provider = PlainTextFinalizerProvider::default();
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

    let PlanReviewRunOutcome::Paused(reason) = outcome else {
        panic!("plain finalizer text must remain a paused review candidate");
    };
    assert!(reason.contains("awaits explicit confirmation"));
    let review = crate::conversation_display::public_plan_review_from_entries(
        parent_session.entries(),
        None,
    )?
    .expect("candidate review should remain visible");
    assert_eq!(
        review.status,
        sigil_kernel::PublicPlanReviewStatus::Finalizing
    );
    let candidate = review.candidate.expect("candidate should be projected");
    assert!(candidate.content.contains("Inspect the live call path"));
    assert_eq!(
        review.allowed_actions,
        Vec::<sigil_kernel::PublicPlanAction>::new()
    );
    let requests = request_tools
        .lock()
        .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
    assert_eq!(requests.len(), 2, "research is followed by one finalizer");
    assert_eq!(
        requests[1],
        vec![sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned()]
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
async fn plan_review_hard_budget_does_not_spawn_an_extra_finalizer() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent_session, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/session.jsonl"),
        )?),
    )?;
    let provider = PlainTextFinalizerProvider::default();
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

    assert!(matches!(outcome, PlanReviewRunOutcome::Paused(_)));
    let requests = request_tools
        .lock()
        .map_err(|_| anyhow!("plan review request recorder lock poisoned"))?;
    assert_eq!(
        requests.len(),
        1,
        "the hard budget must cover research and finalization"
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_finalizer_controls_stay_in_the_finalizer_session() -> Result<()> {
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
                            PlanReviewAttemptStatus::Started | PlanReviewAttemptStatus::Finalizing
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
        "the coordinator commits a finalizer draft to the parent only after it reads child state"
    );

    let finalizer = Session::load_from_store(
        parent_session.provider_name(),
        parent_session.model_name(),
        JsonlSessionStore::new(
            request.finalizer_session_ref.resolve(
                parent_path
                    .parent()
                    .expect("parent session fixture path must have a directory"),
            ),
        )?,
    )?;
    assert!(
        finalizer.entries().iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft))
                if draft.plan_id == request.plan_id
        )),
        "the validated draft source must remain durably owned by the finalizer child"
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_stream_interruption_uses_durable_evidence_for_submit_only_finalization()
-> Result<()> {
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
        vec![sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned()]
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
fn application_plan_decisions_reject_disabled_or_changed_composition_without_mutation() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let config = sigil_kernel::RootConfig::load(&write_admission_test_config(temp.path()))?;
    let session_path = temp.path().join("composition-plan.jsonl");
    let request = seed_revision_decision(&config, temp.path(), &session_path)?;
    let (_, route) = crate::provider_connections::resolve_default_model_route(&config)?;
    let inspected = crate::provider_connections::inspect_session_for_route_resume(
        &config,
        &route,
        JsonlSessionStore::new(&session_path)?,
    )?;
    let scope = inspected.session.session_scope_id().to_owned();
    let baseline = std::fs::read(&session_path)?;
    let mut core = config.clone();
    core.composition = sigil_kernel::RuntimeCompositionConfig::core();
    let mut changed = config.clone();
    changed.composition = sigil_kernel::RuntimeCompositionConfig::new(
        sigil_kernel::RuntimeCompositionProfile::Core,
        [sigil_kernel::OptionalCapability::TaskOrchestration],
    );
    for (selected, expected) in [
        (core, "task orchestration is not selected"),
        (changed, "session capability composition differs"),
    ] {
        for action in [
            crate::ApplicationPlanAction::Run,
            crate::ApplicationPlanAction::Revise,
            crate::ApplicationPlanAction::RetryReview,
        ] {
            let error = crate::application_plan_decision(
                &selected,
                temp.path(),
                &session_path,
                &scope,
                &crate::ApplicationPlanDecisionCommand {
                    plan_id: request.plan_id.as_str().to_owned(),
                    expected_plan_hash: "unused-after-composition-rejection".to_owned(),
                    action,
                    expected_candidate_hash: None,
                    permission_grant: None,
                },
            )
            .expect_err("composition must be checked before plan mutation");
            assert!(error.to_string().contains(expected), "{error:#}");
            assert_eq!(std::fs::read(&session_path)?, baseline);
        }
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
        &config,
        temp.path(),
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
    commit_test_plan_review_draft(
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
    commit_test_plan_review_draft(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        11,
    )?;
    Ok((session, request, draft))
}

#[test]
fn plan_review_recovery_requires_the_recorded_finalizer_binding() -> Result<()> {
    let (mut session, request) = session_with_route_decision()?;
    ensure_test_plan_review_attempt_started(&mut session, &request, 10)?;
    let mut attempt = PlanReviewProjection::from_entries(session.entries())
        .latest_attempt(&request.plan_review_id)
        .cloned()
        .context("started attempt")?;
    let before = session.entries().to_vec();
    attempt.finalizer_session_ref = None;
    let error = crate::plan_review_context_digest_for_attempt(&session, &attempt)
        .expect_err("missing finalizer identity cannot be reconstructed");
    assert!(
        error
            .to_string()
            .contains("executable attempt has no finalizer binding")
    );
    assert_eq!(
        serde_json::to_value(session.entries())?,
        serde_json::to_value(before)?
    );
    Ok(())
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
    commit_test_plan_review_draft(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        11,
    )?;
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
fn plan_revision_guidance_is_durable_before_dispatch_and_retry_uses_a_fresh_attempt() -> Result<()>
{
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
    ensure_test_plan_review_attempt_started(&mut session, &retry, 26)?;
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
        PlanReviewRunOutcome::SubmitOnlyProtocolViolation("wrong tool".to_owned()),
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
            let read = socket.read(&mut buffer).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let body = if request.contains("\"submit_plan_review_result\"") {
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"revision-draft-call\",\"type\":\"function\",\"function\":{\"name\":\"submit_plan_review_result\",\"arguments\":\"{\\\"schema_version\\\":1,\\\"outcome\\\":\\\"draft\\\",\\\"content\\\":\\\"# Revised coordinator migration\\\\n\\\\n1. Revise migration.\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
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
        .expect("revision finalizer must return its exact durable outbox");
    let outcome = execution.outcome;
    let PlanReviewRunOutcome::DraftReady {
        draft: revised_draft,
    } = outcome
    else {
        panic!("revision must commit a draft");
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
async fn revision_provider_construction_failure_commits_confirmed_interrupted_terminal()
-> Result<()> {
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
    let outcome = crate::application_run::execute_plan_review_revision(
        &root_config,
        &workspace_root,
        &session_path,
        &revision_request,
        &mut handler,
        None,
    )
    .await?;
    assert!(matches!(outcome, PlanReviewRunOutcome::Interrupted(_)));

    // The execution future ended and writer recovery found no prior finalizer, so this is a
    // confirmed Interrupted outcome—not a fabricated Failed fallback.
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
        PlanReviewAttemptStatus::Interrupted,
        "provider-construction failure must settle as Interrupted, never Failed"
    );
    assert_eq!(
        attempt.terminal_reason,
        Some(sigil_kernel::PlanReviewTerminalReason::RunInterrupted)
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
    let mut session =
        crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
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

    commit_test_plan_review_draft(
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
    commit_test_plan_review_draft(
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
        &test_plan_compile_input(),
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
    commit_test_plan_review_draft(
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
    commit_test_plan_review_draft(
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
    commit_test_plan_review_draft(
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
    commit_test_plan_review_draft(
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
    commit_test_plan_review_draft(
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
    commit_test_plan_review_draft(
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
max_plan_steps = 64
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
        finalizer_session_ref: Some(SessionRef::new_relative("finalizer-reviewable.jsonl")?),
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
fn current_plan_approval_rejects_old_ready_markers_and_keeps_exact_replay_idempotent() -> Result<()>
{
    let (mut session, draft, _temp) = spine_session("plan_current_only", "Current approval")?;
    let candidate_hash = make_ready(&mut session, &draft)?;
    let mut command = sigil_kernel::PlanRunCommandV1 {
        command_id: "current-plan-approval".to_owned(),
        session_id: session.session_scope_id().to_owned(),
        plan_id: draft.plan_id.clone(),
        expected_plan_hash: draft.plan_hash.clone(),
        expected_candidate_hash: candidate_hash,
        expected_durable_frontier: session.durable_frontier_sequence(),
        start_mode: sigil_kernel::PlanTaskStartMode::CreateAndRun,
        permission: sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy,
        source: sigil_kernel::PlanRunCommandSource::Http,
    };
    let parent_ref = SessionRef::new_relative("parent.jsonl")?;
    let before = session.entries().to_vec();
    assert!(matches!(
        crate::PlanExecutionService::approve(&mut session, parent_ref.clone(), &command, 30),
        Err(sigil_kernel::PlanRunRejectionV1::PlanNotReady { .. }),
    ));
    assert_eq!(
        serde_json::to_value(session.entries())?,
        serde_json::to_value(before)?
    );
    mark_r69_plan_reviewable(&mut session, &draft)?;
    command.expected_durable_frontier = session.durable_frontier_sequence();
    let first =
        crate::PlanExecutionService::approve(&mut session, parent_ref.clone(), &command, 31)
            .map_err(|error| anyhow::anyhow!(crate::plan_run_rejection_message(&error)))?;
    let committed = session.entries().to_vec();
    let replay = crate::PlanExecutionService::approve(&mut session, parent_ref, &command, 32)
        .map_err(|error| anyhow::anyhow!(crate::plan_run_rejection_message(&error)))?;
    assert!(replay.already_approved);
    assert_eq!(first.task_id, replay.task_id);
    assert_eq!(
        serde_json::to_value(session.entries())?,
        serde_json::to_value(committed)?
    );
    Ok(())
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
fn approved_plan_is_directly_executable_without_legacy_materialization() -> Result<()> {
    let (mut session, draft, _temp) = spine_session("plan_r69_materialization", "Materialize")?;
    let candidate_hash = make_ready(&mut session, &draft)?;
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
        finalizer_session_ref: Some(SessionRef::new_relative(
            "finalizer-r69-materialization.jsonl",
        )?),
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
        expected_candidate_hash: candidate_hash,
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
    assert!(artifacts.adoptions.is_empty());
    assert!(artifacts.materializations.is_empty());
    assert!(artifacts.materialization_attempts.is_empty());
    let task = session.task_state_projection();
    let projected = task.tasks.get(&approval.task_id).expect("approved Task");
    assert!(projected.plans.is_empty());
    assert!(projected.direct_execution_admission.is_some());
    assert!(session.entries().iter().all(|entry| !matches!(
        entry,
        SessionLogEntry::Control(
            ControlEntry::TaskMaterializationAttemptStartedV1(_)
                | ControlEntry::TaskMaterializationPreparedV1(_)
        )
    )));
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
    // The legacy candidate-bound adoption API still rejects a reviewable Plan without a
    // candidate. New Run paths use PlanExecutionService::approve and do not call this API.
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
        Ok(_) => panic!("candidate-less plan must be rejected by legacy adoption"),
        Err(rejection) => rejection,
    };
    assert_eq!(rejection.reason_code(), "candidate_missing");
    // The plan is not consumed by rejections.
    assert!(session.plan_artifact_projection().adoptions.is_empty());
    Ok(())
}

#[test]
fn commit_draft_from_child_records_reviewable_text_without_execution_candidate() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session = Session::new("mock", "model");
    let mut session = session.with_store(JsonlSessionStore::new(temp.path().join("spine.jsonl"))?);
    let request = PlanReviewRunRequest {
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
        finalizer_session_ref: SessionRef::new_relative("finalizer.jsonl")?,
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        explicit_objective: Some("implement".to_owned()),
        objective: "implement".to_owned(),
        workspace_snapshot_id: None,
    };
    let draft = draft_entry(&request);
    ensure_test_plan_review_attempt_started(&mut session, &request, 25)?;
    commit_test_plan_review_draft(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        30,
    )?;
    let artifacts = session.plan_artifact_projection();
    assert!(artifacts.latest_candidate(&request.plan_id).is_none());
    assert!(!artifacts.ready_markers.contains_key(&request.plan_id));
    assert!(!artifacts.compile_failures.contains_key(&request.plan_id));
    assert_eq!(
        artifacts.plan_ready_state(&request.plan_id),
        sigil_kernel::PlanReadyStateV1::Ready
    );
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
            if attempt.status == PlanReviewAttemptStatus::DraftReady
    )));
    // Retry is idempotent and does not invent candidate/compiler records.
    commit_test_plan_review_draft(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        31,
    )?;
    let artifacts = session.plan_artifact_projection();
    assert!(artifacts.candidates.is_empty());
    assert!(artifacts.ready_markers.is_empty());
    assert!(artifacts.compile_failures.is_empty());
    Ok(())
}

#[test]
fn incomplete_structured_fields_do_not_trigger_execution_compile() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session = Session::new("mock", "model");
    let mut session = session.with_store(JsonlSessionStore::new(temp.path().join("spine.jsonl"))?);
    let request = PlanReviewRunRequest {
        plan_review_id: sigil_kernel::PlanReviewId::new("review-2")?,
        attempt_id: sigil_kernel::PlanReviewAttemptId::new("attempt-2")?,
        plan_id: sigil_kernel::PlanId::new("plan_spine_5")?,
        source: PlanReviewSource::ExplicitPlanCommand,
        source_turn: ConversationTurnRef {
            session_scope_id: session.session_scope_id().to_owned(),
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
        explicit_objective: Some("implement".to_owned()),
        objective: "implement".to_owned(),
        workspace_snapshot_id: None,
    };
    let mut draft = draft_entry(&request);
    draft.steps[0].isolation = None;
    ensure_test_plan_review_attempt_started(&mut session, &request, 25)?;
    commit_test_plan_review_draft(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        30,
    )?;
    let artifacts = session.plan_artifact_projection();
    assert_eq!(
        artifacts.plan_ready_state(&request.plan_id),
        sigil_kernel::PlanReadyStateV1::Ready
    );
    assert!(!artifacts.ready_markers.contains_key(&request.plan_id));
    assert!(!artifacts.compile_failures.contains_key(&request.plan_id));
    let has_draft_ready_attempt = session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
                if attempt.status == PlanReviewAttemptStatus::DraftReady
        )
    });
    assert!(has_draft_ready_attempt);
    // The plan detail is readable without surfacing an irrelevant compile failure.
    let detail = sigil_kernel::plan_review_detail_from_entries(
        session.entries(),
        &request.plan_id,
        &draft.plan_hash,
    )?;
    assert_eq!(detail.compile.state, sigil_kernel::PlanReadyStateV1::Ready);
    assert!(detail.compile.failure.is_none());
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
fn commit_draft_ignores_advisory_compile_input_drift() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session = Session::new("mock", "model");
    let mut session = session.with_store(JsonlSessionStore::new(temp.path().join("spine.jsonl"))?);
    let request = PlanReviewRunRequest {
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
        finalizer_session_ref: SessionRef::new_relative("finalizer.jsonl")?,
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        explicit_objective: Some("implement".to_owned()),
        objective: "implement".to_owned(),
        workspace_snapshot_id: None,
    };
    let draft = draft_entry(&request);
    ensure_test_plan_review_attempt_started(&mut session, &request, 25)?;
    commit_test_plan_review_draft(
        &mut session,
        &draft,
        &request,
        &test_plan_compile_input(),
        30,
    )?;
    // Compile input is not execution authority, so drift cannot invalidate the same exact draft.
    let mut drifted_input = test_plan_compile_input();
    drifted_input.source_attempt_id = "attempt-drifted".to_owned();
    drifted_input.task_config_contract_hash =
        sigil_kernel::stable_event_uuid("sigil-plan-task-config-v1", "drifted");
    commit_test_plan_review_draft(&mut session, &draft, &request, &drifted_input, 31)?;
    // The durable draft remains unique and no candidate/marker is written.
    let artifacts = session.plan_artifact_projection();
    assert!(artifacts.candidates.is_empty());
    assert!(artifacts.ready_markers.is_empty());
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
    let _environment_guard = crate::test_env::lock();
    let _missing_key = crate::test_env::EnvScope::remove("SIGIL_OPENAI_COMPATIBLE_API_KEY");
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
            valid_for_snapshot: format!("{tag}-snapshot"),
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
    let provider = InvalidThenValidFinalizerProvider::default();
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
    assert!(matches!(outcome, PlanReviewRunOutcome::DraftReady { .. }));
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
async fn plan_review_finalizer_fixed_evidence_precedes_compacted_child_history() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut parent, request) = seed_route_decision(
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            temp.path().join("sessions/parent.jsonl"),
        )?),
    )?;
    for ordinal in 1..=2 {
        let reference = sigil_kernel::plan_review_finalizer_session_ref(
            &request.plan_review_id,
            &request.attempt_id,
            ordinal,
        );
        let store =
            JsonlSessionStore::new(reference.resolve(temp.path().join("sessions").as_path()))?;
        let mut child = Session::new("plan-review-test", "planned-model").with_store(store);
        child.ensure_identity_entry()?;
        seed_completed_plan_input_history(&mut child, "finalizer retained")?;
        compact_plan_input_fixture(&child, &format!("finalizer-{ordinal}"))?;
    }
    let mut research =
        Session::new("plan-review-test", "planned-model").with_store(JsonlSessionStore::new(
            request
                .child_session_ref
                .resolve(temp.path().join("sessions").as_path()),
        )?);
    research.ensure_identity_entry()?;
    research.append_control(ControlEntry::PlanReviewCandidateRecordedV1(Box::new(
        sigil_kernel::plan_review_candidate_recorded_entry(
            request.plan_review_id.clone(),
            request.attempt_id.clone(),
            request.plan_id.clone(),
            request.plan_source_ref(),
            None,
            "# Bounded plan review\n\n1. Implement the bounded change.",
            sigil_kernel::PlanReviewCandidateCompletenessV1::Complete,
            50,
        )?,
    )))?;
    drop(research);
    let provider = InvalidThenValidFinalizerProvider {
        interrupt_research: true,
        ..Default::default()
    };
    let messages = provider.request_messages.clone();
    let recorded_tools = provider.request_tools.clone();
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
    assert!(
        matches!(&outcome, PlanReviewRunOutcome::DraftReady { .. }),
        "unexpected finalizer outcome: {outcome:?}; advertised tools: {:?}; validation feedback: {:?}",
        recorded_tools.lock().expect("recorded tools"),
        messages
            .lock()
            .expect("recorded messages")
            .iter()
            .flat_map(|messages| messages.iter())
            .filter_map(|message| message.content.as_deref())
            .filter(|text| text.starts_with("Previous submit-only attempt"))
            .collect::<Vec<_>>()
    );
    let messages = messages
        .lock()
        .map_err(|_| anyhow!("messages lock poisoned"))?;
    assert_eq!(messages.len(), 3);
    for (ordinal, message_set) in messages.iter().enumerate().skip(1) {
        let checkpoint = message_set
            .iter()
            .position(|message| {
                message
                    .content
                    .as_deref()
                    .is_some_and(|text| text.contains("finalizer retained"))
            })
            .context("child checkpoint missing")?;
        let objective = message_set
            .iter()
            .position(|message| message.content.as_deref() == Some(request.objective.as_str()))
            .context("objective missing")?;
        let evidence = message_set
            .iter()
            .position(|message| {
                message
                    .content
                    .as_deref()
                    .is_some_and(|text| text.starts_with("Bounded host evidence bundle"))
            })
            .context("evidence missing")?;
        let candidate = message_set
            .iter()
            .position(|message| {
                message
                    .content
                    .as_deref()
                    .is_some_and(|text| text.starts_with("Complete Plan candidate"))
            })
            .context("candidate missing")?;
        assert!(objective < evidence && evidence < candidate && candidate < checkpoint);
        if ordinal == 2 {
            let feedback = message_set
                .iter()
                .position(|message| {
                    message
                        .content
                        .as_deref()
                        .is_some_and(|text| text.starts_with("Previous submit-only attempt"))
                })
                .context("feedback missing")?;
            assert!(candidate < feedback && feedback < checkpoint);
            assert!(message_set[feedback].content.as_deref().is_some_and(|text| {
                text.contains("confirm_plan_review_candidate requires decision accept and no other fields")
            }), "candidate confirmation validation must enter the bounded corrective path");
        }
        assert!(!message_set.iter().any(|message| message.content.as_deref() == Some("finalizer retained assistant 0")), "folded child raw messages must not return");
    }
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
    let provider = InvalidThenValidFinalizerProvider::default();
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

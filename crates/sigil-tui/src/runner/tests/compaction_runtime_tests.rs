use std::pin::Pin;

use anyhow::Result;
use async_trait::async_trait;
use fs2::FileExt;
use futures::{Stream, stream};
use sigil_kernel::{
    AgentConfig, CompactionCircuitScopeV1, CompactionFailureEntry, CompactionFailureReason,
    CompactionFallbackParent, CompactionInitiation, CompactionStartedEntry, ControlEntry,
    ConversationInputKind, ConversationInputQueueId, ConversationInputTarget, DurableAuditRecord,
    DurableAuditWriter, DurableEventType, MemoryConfig, ModelMessage,
    PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION, Provider, ProviderCapabilities, ProviderChunk,
    ProviderPhysicalAttemptOutcome, ProviderPhysicalAttemptPurpose,
    ProviderPhysicalAttemptStartedEntry, ProviderPhysicalAttemptTerminalEntry,
    ProviderRequestRejection, ReasoningEffort, RootConfig, SessionConfig, SessionLogEntry,
    StorageRoot, WorkspaceConfig,
};

use crate::runner::worker_loop::queue_conversation_input;
use crate::runner::worker_loop::queue_driver::{
    QueuedConversationCandidatePreparation, prepare_next_queued_conversation_candidate,
};

use super::*;

fn cache_layout_proof(provider_name: &str, model_name: &str) -> sigil_kernel::CacheLayoutProofV1 {
    sigil_kernel::CacheLayoutProofV1::from_request(
        &sigil_kernel::CompletionRequest {
            provider_name: provider_name.to_owned(),
            model_name: model_name.to_owned(),
            messages: vec![ModelMessage::user("current request")],
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(128),
            reasoning_effort: None,
            previous_response_handle: None,
            continuation_states: Vec::new(),
            traffic_partition_key: None,
            background: false,
            store: false,
            deterministic_materialization: true,
            hosted_tools: Vec::new(),
        },
        None,
    )
    .expect("current cache layout proof")
}

const RAW_PROMPT: &str = "inspect https://example.com/private?signature=pre-turn-secret exactly";

fn idle_auto_session(prompt_tokens: u64) -> Session {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.stats_mut().last_prompt_tokens = prompt_tokens;
    session
}

fn requested_idle_auto_state() -> IdleAutoCompactionState {
    let mut state = IdleAutoCompactionState::default();
    state.request_after_successful_chat_run();
    state
}

fn cache_aware_compaction_config() -> sigil_kernel::CompactionConfig {
    sigil_kernel::CompactionConfig {
        context_window_tokens: Some(1_000_000),
        ..sigil_kernel::CompactionConfig::default()
    }
}

fn trusted_cache_context_capabilities() -> sigil_kernel::ProviderContextCapabilities {
    sigil_kernel::ProviderContextCapabilities {
        cache_mode: sigil_kernel::CacheMode::ImplicitPrefix,
        ..sigil_kernel::ProviderContextCapabilities::default()
    }
}

#[test]
fn idle_auto_preflight_rejects_22_percent_without_a_store_or_preparation_handle() {
    let state = requested_idle_auto_state();
    let session = idle_auto_session(216_803);

    let result = idle_auto_compaction_preflight(
        &state,
        Some(&session),
        &cache_aware_compaction_config(),
        &trusted_cache_context_capabilities(),
        IdleAutoCompactionSchedulerEligibility::idle(),
    );

    assert_eq!(
        result.decision,
        IdleAutoCompactionPreflightDecision::NotEligible(
            IdleAutoCompactionNotEligibleReason::NotFitRequired,
        )
    );
    assert_eq!(
        result.evidence,
        IdleAutoCompactionPreflightEvidenceV1 {
            schema_version: 1,
            evaluation_count: 1,
            not_requested_count: 0,
            scheduler_blocked_count: 0,
            not_eligible_count: 1,
            detailed_preparation_candidate_count: 0,
            prompt_tokens: Some(216_803),
            context_window_tokens: Some(1_000_000),
            threshold_status: Some(sigil_kernel::CompactionThresholdStatus::Ready),
            configured_strategy: sigil_kernel::CompactionStrategy::CacheAwareV3,
            effective_strategy: Some(sigil_kernel::CompactionStrategy::CacheAwareV3),
            cache_aware_v3_supported: Some(true),
        }
    );
    assert!(
        state.is_requested(),
        "the pure preflight leaves request consumption to the worker integration"
    );
}

#[test]
fn idle_auto_preflight_keeps_a_requested_run_blocked_at_the_scheduler_boundary() {
    let state = requested_idle_auto_state();
    let session = idle_auto_session(900_000);
    let mut scheduler = IdleAutoCompactionSchedulerEligibility::idle();
    scheduler.run_active = true;

    let result = idle_auto_compaction_preflight(
        &state,
        Some(&session),
        &cache_aware_compaction_config(),
        &trusted_cache_context_capabilities(),
        scheduler,
    );

    assert_eq!(
        result.decision,
        IdleAutoCompactionPreflightDecision::SchedulerBlocked(
            IdleAutoCompactionSchedulerBlockReason::ActiveRun,
        )
    );
    assert_eq!(result.evidence.scheduler_blocked_count, 1);
    assert_eq!(result.evidence.prompt_tokens, None);
    assert_eq!(result.evidence.detailed_preparation_candidate_count, 0);
}

#[test]
fn idle_auto_preflight_admits_soft_pressure_only_to_detailed_preparation() {
    let state = requested_idle_auto_state();
    let session = idle_auto_session(800_000);

    let result = idle_auto_compaction_preflight(
        &state,
        Some(&session),
        &cache_aware_compaction_config(),
        &trusted_cache_context_capabilities(),
        IdleAutoCompactionSchedulerEligibility::idle(),
    );

    assert_eq!(
        result.decision,
        IdleAutoCompactionPreflightDecision::ProceedToDetailedPreparation {
            effective_strategy: sigil_kernel::CompactionStrategy::CacheAwareV3,
        }
    );
    assert_eq!(result.evidence.detailed_preparation_candidate_count, 1);
    assert_eq!(result.evidence.not_eligible_count, 0);
}

#[test]
fn stable_idle_compaction_snapshot_ignores_durable_lifecycle_but_rejects_new_session_entries()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::load_from_store("deepseek", "deepseek-v4-flash", store.clone())?;
    let stable = capture_stable_idle_compaction_snapshot(&session)?
        .expect("empty store-backed session has a stable entry frontier");
    let prepared = stable
        .materialize_compaction_session()?
        .expect("unchanged stable snapshot materializes");

    append_context_window_rejection(
        &store,
        "durable-only-attempt",
        "durable-only-run",
        ProviderPhysicalAttemptPurpose::ConversationGeneration,
        Some(ProviderRequestRejection::ContextWindowExceeded),
    )?;
    assert!(
        capture_stable_idle_compaction_snapshot(&prepared)?.is_some(),
        "provider-attempt lifecycle records do not invalidate session-entry material"
    );

    store.append(&SessionLogEntry::Control(ControlEntry::Note {
        kind: "external-session-entry".to_owned(),
        data: serde_json::Value::Null,
    }))?;
    assert!(
        capture_stable_idle_compaction_snapshot(&prepared)?.is_none(),
        "an externally appended session entry invalidates prepared compaction material"
    );
    Ok(())
}

#[test]
fn eligible_idle_durable_admission_uses_ready_projection_while_data_file_is_locked() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let session = Session::load_from_store("deepseek", "deepseek-v4-flash", store.clone())?;
    let circuit_scope = CompactionCircuitScopeV1 {
        source_cursor_event_id: "source-cursor".to_owned(),
        layout_hash: "layout-hash".to_owned(),
        route_fingerprint: "route-fingerprint".to_owned(),
    };
    let started = store.append_compaction_started(CompactionStartedEntry {
        attempt_id: "idle-failure".to_owned(),
        fallback_parent: CompactionFallbackParent::Root,
        initiation: CompactionInitiation::IdleAutomatic {
            scope_fingerprint: "failed-scope".to_owned(),
            circuit_scope: Some(circuit_scope.clone()),
        },
        base_projection_revision: "projection-r1".to_owned(),
        started_at_unix_ms: 1,
    })?;
    store.append_compaction_failed(CompactionFailureEntry {
        attempt_id: "idle-failure".to_owned(),
        reason: CompactionFailureReason::ValidationFailed,
        failed_at_unix_ms: 2,
    })?;
    let ready = session
        .active_projection_snapshot()?
        .expect("store-backed session keeps its active projection ready");
    assert!(
        ready
            .compaction()
            .has_failed_idle_automatic_scope("failed-scope")
    );
    let owner = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)?;
    owner.lock_exclusive()?;

    let admission = idle_auto_compaction_durable_admission(
        &session,
        "failed-scope",
        circuit_scope,
        false,
        CompactionEmergencyBlockingLayerV1::Unknown,
    )?;
    owner.unlock()?;

    assert!(!started.event_id.is_empty());
    assert!(admission.failure_latched);
    assert_eq!(
        admission.circuit_decision,
        CompactionCircuitBreakerDecisionV1::SameCursorAndLayoutFailed
    );
    Ok(())
}

#[test]
fn eligible_idle_preparation_has_no_path_static_record_reader() {
    let source = include_str!("../worker_loop/compaction_runtime.rs");
    let prepare = source
        .split_once("pub(in crate::runner) fn prepare_idle_auto_compaction")
        .expect("idle preparation function remains present")
        .1
        .split_once("async fn prepare_v2_compaction")
        .expect("portable preparation remains after idle preparation")
        .0;
    assert!(!prepare.contains("JsonlSessionStore::read_event_records"));
    assert!(!prepare.contains("has_failed_idle_automatic_scope("));
}

struct NoopProvider;

#[async_trait]
impl Provider for NoopProvider {
    fn name(&self) -> &str {
        "deepseek"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: sigil_kernel::ReasoningStreamSupport::Unsupported,
            supports_reasoning_effort: false,
            supports_tool_stream: false,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        _request: sigil_kernel::CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter([Ok(ProviderChunk::Done)])))
    }
}

fn append_context_window_rejection(
    store: &JsonlSessionStore,
    physical_attempt_id: &str,
    logical_run_id: &str,
    purpose: ProviderPhysicalAttemptPurpose,
    rejection: Option<ProviderRequestRejection>,
) -> Result<()> {
    let started_event_id = format!("{physical_attempt_id}-start");
    let fingerprint = format!("hmac-sha256:{physical_attempt_id}");
    let started = ProviderPhysicalAttemptStartedEntry {
        schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
        physical_attempt_id: physical_attempt_id.to_owned(),
        logical_run_id: logical_run_id.to_owned(),
        purpose,
        request_material_fingerprint: fingerprint.clone(),
        provider_name: "openai_responses".to_owned(),
        model_name: "gpt-4.1-2025-04-14".to_owned(),
        cache_layout_proof: Some(cache_layout_proof("openai_responses", "gpt-4.1-2025-04-14")),
        started_at_unix_ms: 1,
    };
    let started_record = DurableAuditRecord::new(
        DurableEventType::ProviderPhysicalAttemptStarted,
        serde_json::to_value(started)?,
        physical_attempt_id,
        Some(started_event_id.clone()),
    )?
    .with_event_id(started_event_id.clone())?;
    store.append_and_sync(started_record)?;

    let terminal = ProviderPhysicalAttemptTerminalEntry {
        schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
        physical_attempt_id: physical_attempt_id.to_owned(),
        request_material_fingerprint: fingerprint,
        outcome: ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption,
        rejection,
        provider_request_id: None,
        provider_response_id: None,
        durable_output_event_ids: Vec::new(),
        durable_side_effect_event_ids: Vec::new(),
        finished_at_unix_ms: 2,
    };
    let terminal_record = DurableAuditRecord::new(
        DurableEventType::ProviderPhysicalAttemptTerminal,
        serde_json::to_value(terminal)?,
        physical_attempt_id,
        Some(started_event_id.clone()),
    )?
    .with_event_id(format!("{physical_attempt_id}-terminal"))?
    .with_causation_id(started_event_id)?;
    store.append_and_sync(terminal_record)?;
    Ok(())
}

fn root_config(workspace_root: &std::path::Path, cache_root: &std::path::Path) -> RootConfig {
    RootConfig {
        config_version: 2,
        workspace: WorkspaceConfig {
            root: workspace_root.display().to_string(),
        },
        storage: sigil_kernel::StorageConfig {
            state_root: StorageRoot::Path(workspace_root.join("state").display().to_string()),
            cache_root: StorageRoot::Path(cache_root.display().to_string()),
            mutation_artifact_retention: Default::default(),
            credential_store: Default::default(),
        },
        session: SessionConfig {
            log_dir: Some(workspace_root.join("sessions").display().to_string()),
            retention: Default::default(),
        },
        agent: AgentConfig {
            runtime_provider: "deepseek".to_owned(),
            connection: None,
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: Default::default(),
        memory: MemoryConfig::with_enabled(false),
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        web: Default::default(),
        connections: Default::default(),
        mcp_servers: Vec::new(),
    }
}

#[test]
fn overflow_recovery_accepts_only_one_exact_durable_context_rejection() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session =
        Session::load_from_store("openai_responses", "gpt-4.1-2025-04-14", store.clone())?;
    append_context_window_rejection(
        &store,
        "attempt-exact",
        "foreground-run-1",
        ProviderPhysicalAttemptPurpose::ConversationGeneration,
        Some(ProviderRequestRejection::ContextWindowExceeded),
    )?;

    assert_eq!(
        exact_context_window_rejection_source(&session, "foreground-run-1")?,
        Some("attempt-exact".to_owned())
    );

    append_context_window_rejection(
        &store,
        "attempt-second",
        "foreground-run-1",
        ProviderPhysicalAttemptPurpose::ConversationGeneration,
        Some(ProviderRequestRejection::ContextWindowExceeded),
    )?;
    assert_eq!(
        exact_context_window_rejection_source(&session, "foreground-run-1")?,
        None,
        "multiple provider attempts are never replayed"
    );
    Ok(())
}

#[test]
fn overflow_recovery_rejects_non_context_terminal_evidence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session =
        Session::load_from_store("openai_responses", "gpt-4.1-2025-04-14", store.clone())?;
    append_context_window_rejection(
        &store,
        "attempt-other",
        "foreground-run-2",
        ProviderPhysicalAttemptPurpose::ConversationGeneration,
        None,
    )?;

    assert_eq!(
        exact_context_window_rejection_source(&session, "foreground-run-2")?,
        None
    );
    Ok(())
}

#[test]
fn overflow_recovery_rejects_non_conversation_attempt_evidence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session =
        Session::load_from_store("openai_responses", "gpt-4.1-2025-04-14", store.clone())?;
    append_context_window_rejection(
        &store,
        "attempt-measurement",
        "foreground-run-3",
        ProviderPhysicalAttemptPurpose::InputTokenMeasurement,
        Some(ProviderRequestRejection::ContextWindowExceeded),
    )?;

    assert_eq!(
        exact_context_window_rejection_source(&session, "foreground-run-3")?,
        None
    );
    Ok(())
}

#[test]
fn queued_pre_turn_admission_blocks_without_local_proof_and_never_mutates_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let cache_root = temp.path().join("cache");
    let root_config = root_config(temp.path(), &cache_root);
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Some(Session::load_from_store(
        "deepseek",
        "deepseek-v4-flash",
        store.clone(),
    )?);
    let mut exact_prompts = ExactConversationPromptStore::new();
    queue_conversation_input(
        store.path(),
        &mut session,
        &mut exact_prompts,
        RAW_PROMPT.to_owned(),
        ConversationInputKind::Chat,
        ConversationInputTarget::MainThread,
        ReasoningEffort::High,
    )
    .map_err(anyhow::Error::msg)?;
    let mut session = session.expect("store-backed session should remain available");
    let before_stream = std::fs::read(store.path())?;
    let runtime = tokio::runtime::Runtime::new()?;
    let context_resolver =
        sigil_runtime::RequestContextResolver::request_local(temp.path().to_path_buf());

    let admission = prepare_next_queued_conversation_pre_turn_admission(
        &root_config,
        temp.path(),
        store.path(),
        &NoopProvider,
        &mut session,
        &exact_prompts,
        &MemoryConfig::with_enabled(false),
        Vec::new(),
        None,
        None,
        &context_resolver,
        runtime.handle(),
    )?;

    assert!(matches!(
        admission,
        QueuedConversationPreTurnAdmission::Blocked {
            ref reason,
            candidate: Some(_),
            ..
        }
            if reason == "queued pre-turn exact admission is unavailable from the local token profile"
    ));
    assert_eq!(std::fs::read(store.path())?, before_stream);
    assert!(exact_prompts.contains_key(&ConversationInputQueueId::new("queue_1")?));
    let durable_json = String::from_utf8(before_stream)?;
    assert!(!durable_json.contains("pre-turn-secret"));
    assert!(!durable_json.contains(RAW_PROMPT));
    Ok(())
}

#[test]
fn queued_portable_preflight_with_no_prior_history_is_read_only_and_not_ready() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root_config = root_config(temp.path(), &temp.path().join("cache"));
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Some(Session::load_from_store(
        "deepseek",
        "deepseek-v4-flash",
        store.clone(),
    )?);
    let mut exact_prompts = ExactConversationPromptStore::new();
    queue_conversation_input(
        store.path(),
        &mut session,
        &mut exact_prompts,
        RAW_PROMPT.to_owned(),
        ConversationInputKind::Chat,
        ConversationInputTarget::MainThread,
        ReasoningEffort::High,
    )
    .map_err(anyhow::Error::msg)?;
    let mut session = session.expect("store-backed session should remain available");
    let before_stream = std::fs::read(store.path())?;
    let preparation = prepare_next_queued_conversation_candidate(
        &session,
        &exact_prompts,
        temp.path(),
        &MemoryConfig::with_enabled(false),
        Vec::new(),
        None,
        None,
    )
    .map_err(anyhow::Error::msg)?;
    let QueuedConversationCandidatePreparation::Prepared(candidate) = preparation else {
        panic!("queued chat should materialize an exact pre-turn candidate");
    };

    assert!(
        tokio::runtime::Runtime::new()?
            .block_on(prepare_queued_portable_preflight(
                &root_config,
                temp.path(),
                store.path(),
                &NoopProvider,
                &mut session,
                &MemoryConfig::with_enabled(false),
                *candidate,
            ))?
            .is_none()
    );
    assert_eq!(std::fs::read(store.path())?, before_stream);
    Ok(())
}

#[test]
fn queued_portable_preflight_with_foldable_history_never_starts_without_verified_target_proof()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let root_config = root_config(temp.path(), &temp.path().join("cache"));
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Some(Session::load_from_store(
        "deepseek",
        "deepseek-v4-flash",
        store.clone(),
    )?);
    {
        let stream = session
            .as_mut()
            .expect("store-backed session should remain available");
        for index in 0..3 {
            stream.append_user_message(ModelMessage::user(format!(
                "large completed request {index}: {}",
                "x".repeat(400_000)
            )))?;
            stream.append_assistant_message(ModelMessage::assistant(
                Some(format!("large completed response {index}")),
                Vec::new(),
            ))?;
        }
        stream.append_user_message(ModelMessage::user("older request"))?;
        stream.append_assistant_message(ModelMessage::assistant(
            Some("older response".to_owned()),
            Vec::new(),
        ))?;
        stream.append_user_message(ModelMessage::user("latest durable request"))?;
    }
    let mut exact_prompts = ExactConversationPromptStore::new();
    queue_conversation_input(
        store.path(),
        &mut session,
        &mut exact_prompts,
        RAW_PROMPT.to_owned(),
        ConversationInputKind::Chat,
        ConversationInputTarget::MainThread,
        ReasoningEffort::High,
    )
    .map_err(anyhow::Error::msg)?;
    let mut session = session.expect("store-backed session should remain available");
    let before_stream = std::fs::read(store.path())?;
    let preparation = prepare_next_queued_conversation_candidate(
        &session,
        &exact_prompts,
        temp.path(),
        &MemoryConfig::with_enabled(false),
        Vec::new(),
        None,
        None,
    )
    .map_err(anyhow::Error::msg)?;
    let QueuedConversationCandidatePreparation::Prepared(candidate) = preparation else {
        panic!("queued chat should materialize an exact pre-turn candidate");
    };

    let error = tokio::runtime::Runtime::new()?
        .block_on(prepare_queued_portable_preflight(
            &root_config,
            temp.path(),
            store.path(),
            &NoopProvider,
            &mut session,
            &MemoryConfig::with_enabled(false),
            *candidate,
        ))
        .expect_err("missing local target proof must block pre-turn compaction");

    assert!(
        error
            .to_string()
            .contains("could not resolve DeepSeek transport for portable compaction"),
        "unexpected target-proof failure: {error:#}"
    );
    assert_eq!(std::fs::read(store.path())?, before_stream);
    let durable_json = String::from_utf8(before_stream)?;
    assert!(!durable_json.contains("compaction_started"));
    assert!(!durable_json.contains("pre_turn_pressure"));
    Ok(())
}

#[test]
fn v2_session_portable_transport_uses_its_durable_model_route() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut root_config = root_config(temp.path(), &temp.path().join("cache"));
    root_config.config_version = sigil_kernel::CONFIG_VERSION_V2;
    let connection_id = sigil_kernel::ConnectionId::new("test-default")?;
    root_config.agent.runtime_provider.clear();
    root_config.agent.connection = Some(connection_id.clone());
    root_config.connections.insert(
        connection_id.as_str().to_owned(),
        serde_json::json!({
            "label": "Test default",
            "provider": "deepseek",
            "protocol": "deepseek",
            "base_url": "https://api.deepseek.com",
            "credential": {
                "source": "environment",
                "name": "SIGIL_API_KEY"
            }
        }),
    );
    let model_ref = sigil_kernel::ModelRef::new(connection_id, "deepseek-v4-flash".to_owned())?;
    let (provider_name, route) =
        sigil_runtime::provider_connections::resolve_model_route(&root_config, &model_ref)
            .map_err(anyhow::Error::new)?;
    let session = Session::new_with_route(provider_name, route);

    require_deepseek_portable_transport(&root_config, &session)?;
    Ok(())
}

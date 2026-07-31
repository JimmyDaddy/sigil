use std::{
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, stream};
use sigil_kernel::{
    COMPACTION_TOKEN_PROOF_SCHEMA_VERSION, CacheMode, CompactionAdmissionDecisionV2,
    CompactionAttemptTerminal, CompactionFailureReason, CompactionFoldPlan,
    CompactionForecastConfidenceV1, CompactionForecastSourceV1, CompactionInitiation,
    CompactionLifecycleProjection, CompactionRolloutModeV1, CompletionRequest, ControlEntry,
    EffectiveTokenBudget, ExpectedRemainingTurnsV1, FrozenProviderRequestMaterial,
    InputTokenEvidence, ModelMessage, ModelPricingSnapshotV1, NativeCarrierPortability,
    NativeCompactionCapability, NativeProviderCompactionMaterialization,
    PortableTargetRequestMaterial, Provider, ProviderCapabilities, ProviderChunk,
    ProviderContextCapabilities, ProviderPhysicalAttemptOutcome, ProviderPhysicalAttemptProjection,
    ProviderPhysicalAttemptPurpose, RequestFitProof, Session, SessionLogEntry,
    TokenMeasurementBinding, TokenMeasurementScope, UsageStats, VersionedProfileIdentity,
};

use super::{
    PortableCompactionEconomicsV2Input, SemanticCompactionFallbackPolicy,
    attach_portable_compaction_economics_v2, cache_aware_v3_automatic_supported,
    deepseek_v4_flash_portable_target_material, deepseek_v4_flash_portable_target_proof,
    generate_portable_compaction_summary, is_deepseek_v4_flash_portable_target_profile,
    is_openai_responses_portable_target_profile, materialize_native_compaction_carrier,
    portable_compaction_target_output_tokens, record_semantic_compaction_failure,
};

struct SemanticSummaryProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
    mode: SemanticSummaryMode,
}

fn folding_tail_policy() -> sigil_kernel::AdaptiveTailPolicyV3 {
    sigil_kernel::AdaptiveTailPolicyV3 {
        tail_target_min_tokens: 1,
        tail_target_max_tokens: 1,
        tail_max_usable_context_ratio_ppm: 1_000_000,
        ..sigil_kernel::AdaptiveTailPolicyV3::default()
    }
}

#[derive(Clone, Copy)]
enum SemanticSummaryMode {
    Valid,
    NoUsage,
    Malformed,
    ToolCall,
}

#[async_trait]
impl Provider for SemanticSummaryProvider {
    fn name(&self) -> &str {
        "mock-summary"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: true,
            reports_cache_tokens: true,
            reasoning_stream: sigil_kernel::ReasoningStreamSupport::Unsupported,
            supports_reasoning_effort: false,
            supports_tool_stream: false,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: true,
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
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.requests
            .lock()
            .expect("request lock")
            .push(request.clone());
        if matches!(self.mode, SemanticSummaryMode::Malformed) {
            return Ok(Box::pin(stream::iter([
                Ok(ProviderChunk::TextDelta("not-json".to_owned())),
                Ok(ProviderChunk::Done),
            ])));
        }
        if matches!(self.mode, SemanticSummaryMode::ToolCall) {
            return Ok(Box::pin(stream::iter([
                Ok(ProviderChunk::ToolCallStart {
                    id: "call-1".to_owned(),
                    name: "shell".to_owned(),
                }),
                Ok(ProviderChunk::Done),
            ])));
        }
        let instruction = request
            .messages
            .last()
            .and_then(|message| message.content.as_deref())
            .expect("semantic summary request has a final instruction");
        let source_index = instruction
            .split_once("SOURCE_INDEX=")
            .map(|(_, value)| value)
            .expect("semantic summary instruction has a source index");
        let sources: Vec<serde_json::Value> = serde_json::from_str(source_index)?;
        let source_event_id = sources[0]["event_id"]
            .as_str()
            .expect("source index event id");
        let output = serde_json::json!({
            "in_progress": [],
            "pending_actions": [],
            "provider_continuity": [],
            "model_notes": [{
                "text": "The implementation uses a cache-stable hybrid summary.",
                "source_event_ids": [source_event_id],
                "priority": "normal"
            }]
        })
        .to_string();
        if matches!(self.mode, SemanticSummaryMode::NoUsage) {
            return Ok(Box::pin(stream::iter([
                Ok(ProviderChunk::TextDelta(output)),
                Ok(ProviderChunk::Done),
            ])));
        }
        Ok(Box::pin(stream::iter([
            Ok(ProviderChunk::TextDelta(output)),
            Ok(ProviderChunk::Usage(UsageStats {
                prompt_tokens: 100,
                completion_tokens: 20,
                cache_hit_tokens: 80,
                cache_miss_tokens: 20,
                input_cost: 0.0,
                output_cost: 0.0,
                cache_savings: 0.0,
                system_fingerprint: None,
                cache_usage: None,
                pricing_snapshot: None,
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[tokio::test]
async fn semantic_summary_preserves_stable_prefix_and_records_separate_usage() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = sigil_kernel::JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::load_from_store("mock-summary", "model", store.clone())?;
    for index in 0..4 {
        session.append_user_message(ModelMessage::user(format!("request {index}")))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some(format!("response {index}")),
            Vec::new(),
        ))?;
    }
    session.append_user_message(ModelMessage::user("active request"))?;
    let records = store.read_event_records_writer()?;
    let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &records,
        folding_tail_policy(),
        u64::MAX / 4,
        None,
    )?;
    let before = CompletionRequest {
        provider_name: "mock-summary".to_owned(),
        model_name: "model".to_owned(),
        messages: session.messages(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: Some(8_192),
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: Some("stable-partition".to_owned()),
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    };
    let before_messages = serde_json::to_value(&before.messages)?;
    let frozen = FrozenProviderRequestMaterial::freeze(session.session_scope_id(), before.clone())?;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = SemanticSummaryProvider {
        requests: requests.clone(),
        mode: SemanticSummaryMode::Valid,
    };

    let summary = generate_portable_compaction_summary(
        &provider,
        &mut session,
        &store,
        "portable-summary-test",
        &frozen,
        &plan,
        SemanticCompactionFallbackPolicy::Forbid,
    )
    .await?;

    assert!(!summary.deterministic_emergency_fallback);
    assert_eq!(
        summary.usage.as_ref().map(|usage| usage.cache_hit_tokens),
        Some(80)
    );
    assert_eq!(summary.rebased_plan.folded_event_ids, plan.folded_event_ids);
    assert!(
        summary
            .rebased_plan
            .base_stream_cursor
            .last_applied_stream_sequence
            > plan.base_stream_cursor.last_applied_stream_sequence
    );
    assert_eq!(summary.model_output.model_notes.len(), 1);

    let captured = requests.lock().expect("request lock");
    assert_eq!(captured.len(), 1);
    assert_eq!(
        serde_json::to_value(&captured[0].messages[..before.messages.len()])?,
        before_messages
    );
    assert_eq!(captured[0].messages.len(), before.messages.len() + 1);
    assert!(captured[0].hosted_tools.is_empty());

    let records = store.read_event_records_writer()?;
    let attempts = ProviderPhysicalAttemptProjection::from_records(&records)?;
    let attempts = attempts.attempts_for_logical_run_id("portable-summary-test");
    assert_eq!(attempts.len(), 1);
    assert_eq!(
        attempts[0].entry.purpose,
        ProviderPhysicalAttemptPurpose::SemanticCompaction
    );
    assert_eq!(
        attempts[0]
            .terminal
            .as_ref()
            .map(|terminal| terminal.outcome),
        Some(ProviderPhysicalAttemptOutcome::Completed)
    );
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::SemanticCompactionUsageSnapshot(_))
    )));
    assert_eq!(session.stats().prompt_tokens, 100);
    assert_eq!(
        session.stats().last_prompt_tokens,
        0,
        "an internal summary must not replace ordinary turn pressure"
    );
    let reloaded = Session::load_from_store("mock-summary", "model", store.clone())?;
    assert_eq!(reloaded.stats().prompt_tokens, 100);
    assert_eq!(reloaded.stats().last_prompt_tokens, 0);
    Ok(())
}

fn semantic_summary_fixture() -> Result<(
    tempfile::TempDir,
    sigil_kernel::JsonlSessionStore,
    Session,
    CompactionFoldPlan,
    FrozenProviderRequestMaterial,
)> {
    let temp = tempfile::tempdir()?;
    let store = sigil_kernel::JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("mock-summary", "model").with_store(store.clone());
    for index in 0..4 {
        session.append_user_message(ModelMessage::user(format!("request {index}")))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some(format!("response {index}")),
            Vec::new(),
        ))?;
    }
    session.append_user_message(ModelMessage::user("active request"))?;
    let records = store.read_event_records_writer()?;
    let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &records,
        folding_tail_policy(),
        u64::MAX / 4,
        None,
    )?;
    let frozen = FrozenProviderRequestMaterial::freeze(
        session.session_scope_id(),
        CompletionRequest {
            provider_name: "mock-summary".to_owned(),
            model_name: "model".to_owned(),
            messages: session.messages(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(8_192),
            reasoning_effort: None,
            previous_response_handle: None,
            continuation_states: Vec::new(),
            traffic_partition_key: Some("stable-partition".to_owned()),
            background: false,
            store: false,
            deterministic_materialization: true,
            hosted_tools: Vec::new(),
        },
    )?;
    Ok((temp, store, session, plan, frozen))
}

#[tokio::test]
async fn emergency_summary_failure_is_explicit_and_uses_only_deterministic_floor() -> Result<()> {
    let (_temp, store, mut session, plan, frozen) = semantic_summary_fixture()?;
    let provider = SemanticSummaryProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
        mode: SemanticSummaryMode::Malformed,
    };
    let summary = generate_portable_compaction_summary(
        &provider,
        &mut session,
        &store,
        "portable-summary-emergency",
        &frozen,
        &plan,
        SemanticCompactionFallbackPolicy::DeterministicEmergency,
    )
    .await?;

    assert!(summary.deterministic_emergency_fallback);
    assert!(summary.model_output.in_progress.is_empty());
    assert!(summary.model_output.model_notes.is_empty());
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::Note { kind, data })
            if kind == "semantic_compaction_deterministic_emergency_fallback"
                && data["logical_run_id"] == "portable-summary-emergency"
    )));
    Ok(())
}

#[tokio::test]
async fn emergency_summary_does_not_reuse_usage_from_an_older_attempt() -> Result<()> {
    let (_temp, store, mut session, plan, frozen) = semantic_summary_fixture()?;
    let valid_provider = SemanticSummaryProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
        mode: SemanticSummaryMode::Valid,
    };
    let first = generate_portable_compaction_summary(
        &valid_provider,
        &mut session,
        &store,
        "portable-summary-prior-usage",
        &frozen,
        &plan,
        SemanticCompactionFallbackPolicy::Forbid,
    )
    .await?;
    assert!(first.usage.is_some());

    let malformed_provider = SemanticSummaryProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
        mode: SemanticSummaryMode::Malformed,
    };
    let fallback = generate_portable_compaction_summary(
        &malformed_provider,
        &mut session,
        &store,
        "portable-summary-no-current-usage",
        &frozen,
        &first.rebased_plan,
        SemanticCompactionFallbackPolicy::DeterministicEmergency,
    )
    .await?;

    assert!(fallback.deterministic_emergency_fallback);
    assert!(
        fallback.usage.is_none(),
        "a failed current attempt must not inherit older summary usage"
    );
    Ok(())
}

#[tokio::test]
async fn semantic_summary_tool_call_is_rejected_without_tool_execution() -> Result<()> {
    let (_temp, store, mut session, plan, frozen) = semantic_summary_fixture()?;
    let provider = SemanticSummaryProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
        mode: SemanticSummaryMode::ToolCall,
    };
    let error = generate_portable_compaction_summary(
        &provider,
        &mut session,
        &store,
        "portable-summary-tool-reject",
        &frozen,
        &plan,
        SemanticCompactionFallbackPolicy::Forbid,
    )
    .await
    .expect_err("semantic compaction must not execute a model-requested tool");

    assert!(format!("{error:#}").contains("attempted a client tool call"));
    assert!(!session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(_))
    )));
    Ok(())
}

#[tokio::test]
async fn semantic_summary_without_usage_is_not_admitted() -> Result<()> {
    let (_temp, store, mut session, plan, frozen) = semantic_summary_fixture()?;
    let provider = SemanticSummaryProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
        mode: SemanticSummaryMode::NoUsage,
    };
    let error = generate_portable_compaction_summary(
        &provider,
        &mut session,
        &store,
        "portable-summary-no-usage",
        &frozen,
        &plan,
        SemanticCompactionFallbackPolicy::Forbid,
    )
    .await
    .expect_err("a normal semantic compaction requires observed usage");

    assert!(format!("{error:#}").contains("returned no usage"));
    Ok(())
}

#[tokio::test]
async fn semantic_summary_failure_is_recorded_for_durable_circuit_breaking() -> Result<()> {
    let (_temp, store, mut session, plan, frozen) = semantic_summary_fixture()?;
    let provider = SemanticSummaryProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
        mode: SemanticSummaryMode::Malformed,
    };
    let error = generate_portable_compaction_summary(
        &provider,
        &mut session,
        &store,
        "portable-summary-audited-failure",
        &frozen,
        &plan,
        SemanticCompactionFallbackPolicy::Forbid,
    )
    .await
    .expect_err("malformed summary must fail");

    record_semantic_compaction_failure(
        &store,
        "portable-summary-audited-failure",
        CompactionInitiation::IdleAutomatic {
            scope_fingerprint: "scope-1".to_owned(),
            circuit_scope: None,
        },
        100,
        &error,
    )?;

    let records = store.read_event_records_writer()?;
    let projection = CompactionLifecycleProjection::from_records(&records)?;
    let attempt = projection
        .attempt("portable-summary-audited-failure")
        .expect("audited failure lifecycle");
    assert!(matches!(
        &attempt.terminal,
        Some(CompactionAttemptTerminal::Failed { entry, .. })
            if entry.reason == CompactionFailureReason::SemanticSummaryInvalid
    ));
    Ok(())
}

struct RuntimeNativeCarrierProvider {
    called: Arc<AtomicBool>,
}

#[async_trait]
impl Provider for RuntimeNativeCarrierProvider {
    fn name(&self) -> &str {
        "mock-native"
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

    fn context_capabilities(&self, _model_name: &str) -> ProviderContextCapabilities {
        ProviderContextCapabilities {
            native_compaction: Some(NativeCompactionCapability {
                requires_exact_route_binding: true,
                supports_portable_fallback: true,
            }),
            native_carrier_portability: NativeCarrierPortability::ConnectionModelProtocolBound,
            ..ProviderContextCapabilities::default()
        }
    }

    async fn materialize_native_compaction_carrier(
        &self,
        _session: &Session,
        _logical_run_id: String,
        _frozen_request: FrozenProviderRequestMaterial,
        _covers_through: sigil_kernel::CompactionCursor,
        portable_compaction_id: sigil_kernel::CompactionId,
        _carrier_policy: sigil_kernel::NativeCarrierPolicyV1,
    ) -> Result<Option<NativeProviderCompactionMaterialization>> {
        self.called.store(true, Ordering::SeqCst);
        Ok(Some(NativeProviderCompactionMaterialization {
            physical_attempt_id: "native-attempt".to_owned(),
            observation_id: "native-observation".to_owned(),
            candidate_id: "native-candidate".to_owned(),
            payload_id: "native-payload".to_owned(),
            artifact_id: "native-artifact".to_owned(),
            portable_compaction_id,
            portable_checkpoint_id: "portable-checkpoint".to_owned(),
        }))
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter([Ok(ProviderChunk::Done)])))
    }
}

#[tokio::test]
async fn runtime_dispatches_the_explicit_native_side_only_through_provider_capability() -> Result<()>
{
    let called = Arc::new(AtomicBool::new(false));
    let provider = RuntimeNativeCarrierProvider {
        called: called.clone(),
    };
    let session = Session::new("mock-native", "model");
    let frozen_request = FrozenProviderRequestMaterial::freeze(
        session.session_scope_id(),
        CompletionRequest {
            provider_name: "mock-native".to_owned(),
            model_name: "model".to_owned(),
            messages: vec![ModelMessage::user("portable truth is already durable")],
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(1_024),
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
    let materialized = materialize_native_compaction_carrier(
        &provider,
        &session,
        "native-runtime-test".to_owned(),
        frozen_request,
        sigil_kernel::CompactionCursor {
            session_id: session.session_scope_id().to_owned(),
            through_stream_sequence: 1,
            through_event_id: "portable-event".to_owned(),
        },
        "portable-compaction".to_owned(),
    )
    .await?
    .expect("capable route should return its provider-owned carrier");

    assert!(called.load(Ordering::SeqCst));
    assert_eq!(materialized.candidate_id, "native-candidate");
    Ok(())
}

fn request(content: &str) -> CompletionRequest {
    CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        messages: vec![ModelMessage::user(content)],
        tools: Vec::new(),
        temperature: None,
        max_tokens: Some(10_000),
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    }
}

#[test]
fn cache_aware_v3_automatic_requires_both_exact_profile_and_trusted_route_capability() {
    let trusted = ProviderContextCapabilities {
        cache_mode: CacheMode::ImplicitPrefix,
        ..ProviderContextCapabilities::default()
    };
    assert!(cache_aware_v3_automatic_supported(
        "deepseek",
        "deepseek-v4-flash",
        &trusted
    ));
    assert!(!cache_aware_v3_automatic_supported(
        "deepseek",
        "deepseek-v4-flash",
        &ProviderContextCapabilities::unknown()
    ));
    assert!(!cache_aware_v3_automatic_supported(
        "openai_compat",
        "deepseek-v4-flash",
        &trusted
    ));
    assert!(
        !cache_aware_v3_automatic_supported("openai_responses", "gpt-4.1", &trusted),
        "provider-side token measurement is not admitted in the idle automatic path"
    );
}

fn profile(id: &str) -> VersionedProfileIdentity {
    VersionedProfileIdentity::from_content(id, 1, id.as_bytes())
}

fn exact_binding() -> TokenMeasurementBinding {
    TokenMeasurementBinding {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        wire_profile: profile("wire"),
        token_measurement_profile: profile("tokenizer"),
        hosted_parity_profile: Some(profile("parity")),
    }
}

fn exact_evidence(
    tokens: u64,
    material: &FrozenProviderRequestMaterial,
    binding: &TokenMeasurementBinding,
) -> InputTokenEvidence {
    InputTokenEvidence::Exact {
        tokens,
        material_fingerprint: material.fingerprint().to_owned(),
        measurement_scope: TokenMeasurementScope::RenderedTargetInput,
        binding: binding.clone(),
        provider_model_snapshot: None,
        provider_system_fingerprint: None,
    }
}

#[test]
fn portable_target_admission_requires_a_preinstalled_verified_tokenizer() -> Result<()> {
    let cache = tempfile::tempdir()?;
    let frozen = FrozenProviderRequestMaterial::freeze(
        "portable-runtime-test-session",
        CompletionRequest {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
            messages: vec![ModelMessage::user("continue the task")],
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(32_768),
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

    let error = deepseek_v4_flash_portable_target_material(cache.path(), frozen)
        .expect_err("a missing local tokenizer must keep compaction unavailable");
    assert!(
        error
            .to_string()
            .contains("verified DeepSeek V4 tokenizer is unavailable")
    );
    Ok(())
}

#[test]
fn portable_target_profile_is_explicit_and_proof_never_falls_back_to_a_default() -> Result<()> {
    assert!(is_deepseek_v4_flash_portable_target_profile(
        "deepseek",
        "deepseek-v4-flash"
    ));
    assert!(!is_deepseek_v4_flash_portable_target_profile(
        "deepseek",
        "deepseek-v4-pro"
    ));
    assert!(!is_deepseek_v4_flash_portable_target_profile(
        "openai_compat",
        "deepseek-v4-flash"
    ));
    assert!(is_openai_responses_portable_target_profile(
        "openai_responses",
        "gpt-4.1-2025-04-14"
    ));
    assert!(!is_openai_responses_portable_target_profile(
        "openai_responses",
        "gpt-4.1"
    ));
    assert!(!is_openai_responses_portable_target_profile(
        "openai_compat",
        "gpt-4.1-2025-04-14"
    ));

    let cache = tempfile::tempdir()?;
    let frozen = FrozenProviderRequestMaterial::freeze(
        "portable-runtime-test-session",
        CompletionRequest {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
            messages: vec![ModelMessage::user("continue the task")],
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
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

    let error = deepseek_v4_flash_portable_target_proof(cache.path(), &frozen)
        .expect_err("an omitted output cap must never be inferred");
    assert!(
        error
            .to_string()
            .contains("requires explicit max_tokens=32768")
    );
    Ok(())
}

#[test]
fn portable_target_output_reservation_is_explicit_for_each_admitted_profile() {
    assert_eq!(
        portable_compaction_target_output_tokens("deepseek", "deepseek-v4-flash"),
        Some(32_768)
    );
    assert_eq!(
        portable_compaction_target_output_tokens("openai_responses", "gpt-4.1-2025-04-14"),
        Some(32_768)
    );
    assert_eq!(
        portable_compaction_target_output_tokens("openai_responses", "gpt-4.1"),
        None
    );
}

#[test]
fn runtime_attaches_forecast_and_trusted_cost_to_the_existing_portable_proof() -> Result<()> {
    let binding = exact_binding();
    let before = FrozenProviderRequestMaterial::freeze(
        "portable-economics-session",
        request("large stable prefix"),
    )?;
    let after =
        FrozenProviderRequestMaterial::freeze("portable-economics-session", request("checkpoint"))?;
    let proof = RequestFitProof {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        input: exact_evidence(10_000, &after, &binding),
        budget: EffectiveTokenBudget {
            schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            budget_profile: profile("budget"),
            context_window_tokens: 100_000,
            requested_output_tokens: 10_000,
            safety_buffer_tokens: 5_000,
        },
    };
    let material = PortableTargetRequestMaterial::new(after, binding.clone(), proof)
        .with_portable_economics(&before, exact_evidence(60_000, &before, &binding))?;
    let attached = attach_portable_compaction_economics_v2(
        material,
        PortableCompactionEconomicsV2Input {
            next_turn_p95_tokens: 5_000,
            tool_growth_p95_tokens: 2_000,
            provider_state_tokens: 1_000,
            bulky_shrink_candidate_tokens: 0,
            overflow_observed: false,
            expected_remaining_turns: ExpectedRemainingTurnsV1 {
                turns: 3,
                source: CompactionForecastSourceV1::SessionTurnShape,
                confidence: CompactionForecastConfidenceV1::Medium,
                source_event_ids: Vec::new(),
            },
            observed_current_cache_read_tokens: Some(50_000),
            observed_current_uncached_tokens: Some(10_000),
            pricing_snapshot: Some(ModelPricingSnapshotV1 {
                schema_version: ModelPricingSnapshotV1::SCHEMA_VERSION,
                snapshot_id: "runtime-test-price".to_owned(),
                currency: "USD".to_owned(),
                unit_tokens: 1_000_000,
                cache_read_per_unit: 0.1,
                cache_write_per_unit: None,
                uncached_input_per_unit: 1.0,
                output_per_unit: 2.0,
                source: "provider-owned-test-catalog".to_owned(),
                verified_at: "2026-07-28".to_owned(),
            }),
            compactor_usage_observed: true,
            compactor_cache_read_tokens: 0,
            compactor_uncached_input_tokens: 0,
            compactor_output_tokens: 0,
            rollout_mode: CompactionRolloutModeV1::Preview,
            user_confirmed: true,
        },
    )?;
    let extension = attached
        .portable_economics()
        .and_then(|economics| economics.v2_economics.as_ref())
        .expect("RFC-0057 economics extension");

    assert_eq!(extension.token_savings, 50_000);
    assert_eq!(
        extension
            .cost_projection
            .as_ref()
            .expect("trusted price projection")
            .pricing
            .snapshot_id,
        "runtime-test-price"
    );
    assert_eq!(
        extension.admission.decision,
        CompactionAdmissionDecisionV2::Admit
    );
    Ok(())
}

#[test]
fn fit_required_bypasses_cost_savings_threshold_but_remains_exactly_proven() -> Result<()> {
    let binding = exact_binding();
    let before =
        FrozenProviderRequestMaterial::freeze("fit-bypass-session", request("before request"))?;
    let after =
        FrozenProviderRequestMaterial::freeze("fit-bypass-session", request("after request"))?;
    let proof = RequestFitProof {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        input: exact_evidence(59_990, &after, &binding),
        budget: EffectiveTokenBudget {
            schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            budget_profile: profile("fit-bypass-budget"),
            context_window_tokens: 70_000,
            requested_output_tokens: 5_000,
            safety_buffer_tokens: 1_000,
        },
    };
    let candidate = PortableTargetRequestMaterial::new(after, binding.clone(), proof)
        .with_portable_economics_v2_candidate(&before, exact_evidence(60_000, &before, &binding))?;
    let attached = attach_portable_compaction_economics_v2(
        candidate,
        PortableCompactionEconomicsV2Input {
            next_turn_p95_tokens: 10_000,
            tool_growth_p95_tokens: 1_000,
            provider_state_tokens: 0,
            bulky_shrink_candidate_tokens: 0,
            overflow_observed: false,
            expected_remaining_turns: ExpectedRemainingTurnsV1 {
                turns: 1,
                source: CompactionForecastSourceV1::ConservativeFallback,
                confidence: CompactionForecastConfidenceV1::Low,
                source_event_ids: Vec::new(),
            },
            observed_current_cache_read_tokens: None,
            observed_current_uncached_tokens: None,
            pricing_snapshot: None,
            compactor_usage_observed: false,
            compactor_cache_read_tokens: 0,
            compactor_uncached_input_tokens: 0,
            compactor_output_tokens: 0,
            rollout_mode: CompactionRolloutModeV1::Automatic,
            user_confirmed: false,
        },
    )?;
    let extension = attached
        .portable_economics()
        .and_then(|economics| economics.v2_economics.as_ref())
        .expect("fit-required extension");

    assert_eq!(extension.token_savings, 10);
    assert!(extension.forecast.fit_required);
    assert_eq!(
        extension.admission.decision,
        CompactionAdmissionDecisionV2::Admit
    );
    assert!(extension.admission.automatic_allowed);
    Ok(())
}

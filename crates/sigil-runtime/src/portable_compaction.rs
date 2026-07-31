use std::path::Path;

use anyhow::{Context, Result, bail};
use sigil_kernel::{
    CompactionAdmissionOptionsV2, CompactionCostModelInputV1, CompactionCursor,
    CompactionEconomicsPolicyV1, CompactionEconomicsV2, CompactionFailureEntry,
    CompactionFailureReason, CompactionFallbackParent, CompactionFitForecastInputV1,
    CompactionFitForecastV1, CompactionFoldPlan, CompactionInitiation, CompactionRolloutModeV1,
    CompactionStartedEntry, ContinuationModelOutputV1, ControlEntry, DurableEventType,
    ExpectedRemainingTurnsV1, FrozenProviderRequestMaterial, JsonlSessionStore,
    ModelPricingSnapshotV1, NativeCarrierPolicyV1, NativeProviderCompactionMaterialization,
    PortableTargetRequestMaterial, Provider, ProviderRetentionPolicyV1, RequestFitProof, Session,
    SessionLogEntry, SessionStreamRecord, TokenMeasurementBinding, TrustedCompactionPricingV1,
};
use sigil_provider_deepseek::{
    DEFAULT_DEEPSEEK_V4_FLASH_MODEL, DeepSeekProviderConfig,
    DeepSeekV4FlashPortableTargetAdmission, DeepSeekV4FlashTokenCounter, StrictToolsMode,
    default_deepseek_v4_flash_portable_target_output_tokens,
    default_deepseek_v4_flash_tokenizer_cache_path, download_default_deepseek_v4_flash_tokenizer,
};
use sigil_provider_openai_responses::{
    OPENAI_RESPONSES_PORTABLE_TARGET_MODEL, OPENAI_RESPONSES_PORTABLE_TARGET_OUTPUT_TOKENS,
};

/// Output budget used by the single default semantic-compaction request.
pub const SEMANTIC_COMPACTION_MAX_OUTPUT_TOKENS: u32 = 4_096;

/// Whether a failed semantic summary may activate the deterministic continuity floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticCompactionFallbackPolicy {
    Forbid,
    DeterministicEmergency,
}

/// Validated process-local semantic summary and the plan rebased over its audit records.
#[derive(Debug, Clone)]
pub struct PortableCompactionSummary {
    pub model_output: ContinuationModelOutputV1,
    pub usage: Option<sigil_kernel::UsageStats>,
    pub rebased_plan: CompactionFoldPlan,
    pub deterministic_emergency_fallback: bool,
}

/// Calls the current route once to produce a strict semantic summary while preserving the old
/// request as the message/tool prefix.
///
/// The call is durably audited as `SemanticCompaction`. Its own audit records are then admitted
/// into a rebased fold plan only when no provider-visible fold source changed concurrently.
pub async fn generate_portable_compaction_summary(
    provider: &dyn Provider,
    session: &mut Session,
    store: &JsonlSessionStore,
    logical_run_id: &str,
    frozen_before_request: &FrozenProviderRequestMaterial,
    plan: &CompactionFoldPlan,
    fallback_policy: SemanticCompactionFallbackPolicy,
) -> Result<PortableCompactionSummary> {
    let source_records = store.read_event_records_writer()?;
    let instruction = sigil_kernel::build_semantic_compaction_instruction(&source_records, plan)?;
    let mut summary_request = frozen_before_request.request().clone();
    let original_message_count = summary_request.messages.len();
    let original_messages = serde_json::to_value(&summary_request.messages)
        .context("failed to encode current epoch message prefix")?;
    summary_request.messages.push(instruction);
    summary_request.max_tokens = Some(SEMANTIC_COMPACTION_MAX_OUTPUT_TOKENS);
    summary_request.background = false;
    summary_request.hosted_tools.clear();
    if serde_json::to_value(&summary_request.messages[..original_message_count])
        .context("failed to encode semantic compaction message prefix")?
        != original_messages
    {
        bail!("semantic compaction request rewrote the current epoch message prefix");
    }
    let frozen_summary_request =
        FrozenProviderRequestMaterial::freeze(session.session_scope_id(), summary_request)?;
    let generated = sigil_kernel::generate_semantic_compaction(
        provider,
        session,
        logical_run_id,
        frozen_summary_request,
    )
    .await
    .and_then(|generation| {
        let usage = generation
            .usage
            .context("semantic compaction provider returned no usage")?;
        if usage.prompt_tokens == 0 || usage.completion_tokens == 0 {
            bail!("semantic compaction provider returned incomplete usage");
        }
        let output = sigil_kernel::parse_semantic_compaction_output(&generation.output_text)?;
        validate_semantic_compaction_sources(&output, plan)?;
        Ok((output, Some(usage)))
    });
    match generated {
        Ok((model_output, usage)) => {
            let rebased_plan = refresh_plan_after_semantic_compaction(
                store,
                plan,
                &source_records,
                logical_run_id,
                false,
            )?;
            Ok(PortableCompactionSummary {
                model_output,
                usage,
                rebased_plan,
                deterministic_emergency_fallback: false,
            })
        }
        Err(error) if fallback_policy == SemanticCompactionFallbackPolicy::Forbid => Err(error),
        Err(_) => {
            session.append_control(ControlEntry::Note {
                kind: "semantic_compaction_deterministic_emergency_fallback".to_owned(),
                data: serde_json::json!({
                    "logical_run_id": logical_run_id,
                    "reason": "semantic_summary_unavailable"
                }),
            })?;
            let rebased_plan = refresh_plan_after_semantic_compaction(
                store,
                plan,
                &source_records,
                logical_run_id,
                true,
            )?;
            let usage = semantic_compaction_usage_after(store, source_records.len())?;
            Ok(PortableCompactionSummary {
                model_output: ContinuationModelOutputV1 {
                    in_progress: Vec::new(),
                    pending_actions: Vec::new(),
                    provider_continuity: Vec::new(),
                    model_notes: Vec::new(),
                },
                usage,
                rebased_plan,
                deterministic_emergency_fallback: true,
            })
        }
    }
}

/// Records a normal-path semantic-summary failure in the same durable compaction lifecycle used
/// by the idle circuit breaker.
///
/// Unsupported routes are rejected before the summary call and must not use this helper.
///
/// # Errors
///
/// Returns an error when the failure lifecycle cannot be appended atomically.
pub fn record_semantic_compaction_failure(
    store: &JsonlSessionStore,
    attempt_id: &str,
    initiation: CompactionInitiation,
    started_at_unix_ms: u64,
    error: &anyhow::Error,
) -> Result<()> {
    let reason = semantic_compaction_failure_reason(error);
    store.append_compaction_started(CompactionStartedEntry {
        attempt_id: attempt_id.to_owned(),
        fallback_parent: CompactionFallbackParent::Root,
        initiation,
        base_projection_revision: "portable-v3-hybrid-summary-r1".to_owned(),
        started_at_unix_ms,
    })?;
    store.append_compaction_failed(CompactionFailureEntry {
        attempt_id: attempt_id.to_owned(),
        reason,
        failed_at_unix_ms: crate::current_unix_time_ms(),
    })?;
    Ok(())
}

fn semantic_compaction_failure_reason(error: &anyhow::Error) -> CompactionFailureReason {
    let normalized = error
        .chain()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if normalized.contains("timed out") || normalized.contains("timeout") {
        CompactionFailureReason::SemanticSummaryTimeout
    } else if normalized.contains("bounded size")
        || normalized.contains("item limit")
        || normalized.contains("too large")
        || normalized.contains("output limit")
    {
        CompactionFailureReason::SemanticSummaryInflated
    } else {
        CompactionFailureReason::SemanticSummaryInvalid
    }
}

fn semantic_compaction_usage_after(
    store: &JsonlSessionStore,
    source_record_count: usize,
) -> Result<Option<sigil_kernel::UsageStats>> {
    let records = store.read_event_records_writer()?;
    records
        .get(source_record_count..)
        .context("semantic compaction usage source frontier exceeds durable history")?
        .iter()
        .filter(|record| {
            record.stored_event().event_kind() == Some(DurableEventType::SessionEntryRecorded)
        })
        .try_fold(None, |latest, record| {
            let entry: SessionLogEntry = serde_json::from_value(
                record
                    .stored_event()
                    .payload
                    .get("session_log_entry")
                    .cloned()
                    .context("semantic compaction usage record has no session entry")?,
            )
            .context("failed to decode semantic compaction usage record")?;
            Ok(match entry {
                SessionLogEntry::Control(ControlEntry::SemanticCompactionUsageSnapshot(usage)) => {
                    Some(usage)
                }
                _ => latest,
            })
        })
}

fn validate_semantic_compaction_sources(
    output: &ContinuationModelOutputV1,
    plan: &CompactionFoldPlan,
) -> Result<()> {
    let allowed = plan
        .folded_event_ids
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    for item in [
        output.in_progress.as_slice(),
        output.pending_actions.as_slice(),
        output.provider_continuity.as_slice(),
        output.model_notes.as_slice(),
    ]
    .into_iter()
    .flatten()
    {
        let mut cited = std::collections::BTreeSet::new();
        for event_id in &item.source_event_ids {
            if !allowed.contains(event_id) {
                bail!("semantic compaction output references a source outside the closed index");
            }
            if !cited.insert(event_id) {
                bail!("semantic compaction output duplicates one item source");
            }
        }
    }
    Ok(())
}

fn refresh_plan_after_semantic_compaction(
    store: &JsonlSessionStore,
    original_plan: &CompactionFoldPlan,
    source_records: &[SessionStreamRecord],
    logical_run_id: &str,
    deterministic_emergency_fallback: bool,
) -> Result<CompactionFoldPlan> {
    let records = store.read_event_records_writer()?;
    if records.len() <= source_records.len() {
        bail!("semantic compaction physical attempt did not advance the durable stream");
    }
    let interleaved = &records[source_records.len()..];
    if interleaved
        .first()
        .and_then(|record| record.stored_event().event_kind())
        != Some(DurableEventType::ProviderPhysicalAttemptStarted)
    {
        bail!("semantic compaction source changed outside its physical-attempt lifecycle");
    }
    let terminal_index = interleaved
        .iter()
        .position(|record| {
            record.stored_event().event_kind()
                == Some(DurableEventType::ProviderPhysicalAttemptTerminal)
        })
        .context("semantic compaction physical attempt has no terminal")?;
    for (index, record) in interleaved.iter().enumerate().skip(1) {
        if index == terminal_index {
            continue;
        }
        if record.stored_event().event_kind() != Some(DurableEventType::SessionEntryRecorded) {
            bail!("semantic compaction source changed while the summary was generated");
        }
        let entry: SessionLogEntry = serde_json::from_value(
            record
                .stored_event()
                .payload
                .get("session_log_entry")
                .cloned()
                .context("semantic compaction usage record has no session entry")?,
        )
        .context("failed to decode semantic compaction usage record")?;
        let semantic_usage = matches!(
            &entry,
            SessionLogEntry::Control(ControlEntry::SemanticCompactionUsageSnapshot(_))
        );
        let emergency_fallback = matches!(
            &entry,
            SessionLogEntry::Control(ControlEntry::Note { kind, data })
                if deterministic_emergency_fallback
                    && kind == "semantic_compaction_deterministic_emergency_fallback"
                    && data.get("logical_run_id").and_then(serde_json::Value::as_str)
                        == Some(logical_run_id)
        );
        if !semantic_usage && !emergency_fallback {
            bail!("semantic compaction source changed while the summary was generated");
        }
    }
    let physical_attempts =
        sigil_kernel::ProviderPhysicalAttemptProjection::from_records(&records)?;
    let attempts = physical_attempts.attempts_for_logical_run_id(logical_run_id);
    if attempts.len() != 1
        || attempts[0].entry.purpose
            != sigil_kernel::ProviderPhysicalAttemptPurpose::SemanticCompaction
        || attempts[0].terminal.as_ref().is_none()
        || (!deterministic_emergency_fallback
            && attempts[0].terminal.as_ref().is_none_or(|terminal| {
                terminal.outcome != sigil_kernel::ProviderPhysicalAttemptOutcome::Completed
            }))
    {
        bail!("semantic compaction physical-attempt lineage is incomplete");
    }
    let adaptive = &original_plan.adaptive_tail;
    let refreshed = CompactionFoldPlan::from_records_after_adaptive_tail(
        &records,
        adaptive.policy.clone(),
        adaptive.exact_fit_limit_tokens,
        original_plan.prior_folded_through.as_ref(),
    )?;
    if refreshed.folded_event_ids != original_plan.folded_event_ids {
        bail!("semantic compaction fold set changed while the summary was generated");
    }
    Ok(refreshed)
}

/// Runtime-owned evidence needed to extend the existing exact portable token proof with RFC-0057
/// fit and cache-aware economics.
#[derive(Debug, Clone)]
pub struct PortableCompactionEconomicsV2Input {
    pub next_turn_p95_tokens: u64,
    pub tool_growth_p95_tokens: u64,
    pub provider_state_tokens: u64,
    pub bulky_shrink_candidate_tokens: u64,
    pub overflow_observed: bool,
    pub expected_remaining_turns: ExpectedRemainingTurnsV1,
    pub observed_current_cache_read_tokens: Option<u64>,
    pub observed_current_uncached_tokens: Option<u64>,
    pub pricing_snapshot: Option<ModelPricingSnapshotV1>,
    /// Whether the provider emitted complete usage for the semantic-summary request.
    ///
    /// When false, the numeric fields below are placeholders only and no monetary projection may
    /// be produced from them.
    pub compactor_usage_observed: bool,
    pub compactor_cache_read_tokens: u64,
    pub compactor_uncached_input_tokens: u64,
    pub compactor_output_tokens: u64,
    pub rollout_mode: CompactionRolloutModeV1,
    pub user_confirmed: bool,
}

/// Returns whether this exact runtime provider route can execute a native acceleration carrier.
///
/// Capability alone does not authorize the additional request; callers must also require the
/// explicit `compaction.native_carrier_enabled` user setting.
pub fn native_compaction_carrier_supported(provider: &dyn Provider, model_name: &str) -> bool {
    let capabilities = provider.context_capabilities(model_name);
    capabilities.validate().is_ok() && capabilities.native_compaction.is_some()
}

/// Executes the optional native side of portable/native dual-write.
///
/// The portable checkpoint identified by `portable_compaction_id` must already be durable. The
/// provider driver records its own physical-attempt lifecycle and encrypted payload candidate.
/// This function never activates the resulting carrier.
pub async fn materialize_native_compaction_carrier(
    provider: &dyn Provider,
    session: &Session,
    logical_run_id: String,
    frozen_request: FrozenProviderRequestMaterial,
    covers_through: CompactionCursor,
    portable_compaction_id: sigil_kernel::CompactionId,
) -> Result<Option<NativeProviderCompactionMaterialization>> {
    if provider.name() != session.provider_name() {
        bail!("native compaction provider does not match the durable session");
    }
    if !native_compaction_carrier_supported(provider, session.model_name()) {
        bail!("provider-native compaction carrier is unavailable on this exact route");
    }
    let request_store_mode = frozen_request.request().store;
    let carrier_policy = NativeCarrierPolicyV1 {
        provider_retention: if request_store_mode {
            ProviderRetentionPolicyV1::Allowed
        } else {
            ProviderRetentionPolicyV1::Disallowed
        },
        request_store_mode,
        expires_at_unix_ms: None,
    };
    provider
        .materialize_native_compaction_carrier(
            session,
            logical_run_id,
            frozen_request,
            covers_through,
            portable_compaction_id,
            carrier_policy,
        )
        .await
}

/// Attaches one forecast/cost/admission record to the already-proven portable before/after proof.
///
/// Unknown cache usage or pricing produces no money projection; it never fabricates a zero-cost
/// category. The token-only fallback remains manual, while fit-required admission can still
/// become automatic.
///
/// # Errors
///
/// Returns an error when the base proof is absent, the forecast is invalid, observed cache
/// categories are inconsistent, or the extension drifts from exact before/after savings.
pub fn attach_portable_compaction_economics_v2(
    material: PortableTargetRequestMaterial,
    input: PortableCompactionEconomicsV2Input,
) -> Result<PortableTargetRequestMaterial> {
    let economics = material
        .portable_economics()
        .context("portable target material has no exact before/after economics proof")?;
    let current_input_tokens = economics.before_input.admission_tokens();
    let post_rotation_input_tokens = material.proof().input.admission_tokens();
    let forecast = CompactionFitForecastV1::from_input(CompactionFitForecastInputV1 {
        context_window_tokens: material.proof().budget.context_window_tokens,
        current_input_tokens,
        next_turn_p95_tokens: input.next_turn_p95_tokens,
        reserved_output_tokens: material.proof().budget.requested_output_tokens,
        tool_growth_p95_tokens: input.tool_growth_p95_tokens,
        provider_state_tokens: input.provider_state_tokens,
        safety_buffer_tokens: material.proof().budget.safety_buffer_tokens,
        bulky_shrink_candidate_tokens: input.bulky_shrink_candidate_tokens,
        overflow_observed: input.overflow_observed,
        expected_remaining_turns: input.expected_remaining_turns,
    })?;
    let pricing_and_input = if !input.compactor_usage_observed {
        None
    } else {
        match (
            input.pricing_snapshot.as_ref(),
            input.observed_current_cache_read_tokens,
            input.observed_current_uncached_tokens,
        ) {
            (Some(snapshot), Some(observed_read), Some(observed_uncached)) => {
                let observed_total = observed_read
                    .checked_add(observed_uncached)
                    .context("observed cache token total overflowed")?;
                if observed_total == 0 {
                    None
                } else {
                    let scaled_read = u64::try_from(
                        u128::from(current_input_tokens)
                            .checked_mul(u128::from(observed_read))
                            .context("scaled cache-read numerator overflowed")?
                            / u128::from(observed_total),
                    )
                    .context("scaled cache-read tokens exceed u64")?;
                    let scaled_uncached = current_input_tokens
                        .checked_sub(scaled_read)
                        .context("scaled cache-read tokens exceed current input")?;
                    let observed_hit_ratio_ppm = u32::try_from(
                        u128::from(observed_read)
                            .checked_mul(1_000_000)
                            .context("cache-hit ratio numerator overflowed")?
                            / u128::from(observed_total),
                    )
                    .context("cache-hit ratio exceeds u32")?;
                    Some((
                        TrustedCompactionPricingV1::from_model_snapshot(snapshot)?,
                        CompactionCostModelInputV1 {
                            current_cache_read_tokens: scaled_read,
                            current_uncached_input_tokens: scaled_uncached,
                            post_rotation_input_tokens,
                            next_turn_p95_tokens: input.next_turn_p95_tokens,
                            compactor_cache_read_tokens: input.compactor_cache_read_tokens,
                            compactor_uncached_input_tokens: input.compactor_uncached_input_tokens,
                            compactor_output_tokens: input.compactor_output_tokens,
                            cache_scenario: Some(sigil_kernel::CompactionCacheScenarioV1 {
                                current_epoch_hit_ratio_ppm: observed_hit_ratio_ppm,
                                rotated_epoch_hit_ratio_ppm: observed_hit_ratio_ppm,
                                current_epoch_ttl_turns: None,
                                rotated_epoch_ttl_turns: None,
                            }),
                        },
                    ))
                }
            }
            (None, _, _) | (_, None, _) | (_, _, None) => None,
        }
    };
    let extension = CompactionEconomicsV2::evaluate(
        forecast,
        CompactionEconomicsPolicyV1::default(),
        pricing_and_input,
        economics.savings_tokens,
        economics.savings_ratio_ppm,
        CompactionAdmissionOptionsV2 {
            rollout_mode: input.rollout_mode,
            user_confirmed: input.user_confirmed,
        },
    )?;
    material.with_compaction_economics_v2(extension)
}

/// Returns the explicit output cap used by the admitted DeepSeek V4 portable target request.
#[must_use]
pub const fn deepseek_v4_flash_portable_target_output_tokens() -> u32 {
    default_deepseek_v4_flash_portable_target_output_tokens()
}

/// Returns whether a request identity is admitted by the first exact portable-target profile.
#[must_use]
pub fn is_deepseek_v4_flash_portable_target_profile(provider_name: &str, model_name: &str) -> bool {
    provider_name == "deepseek" && model_name == DEFAULT_DEEPSEEK_V4_FLASH_MODEL
}

/// Returns whether a request identity is the only OpenAI Responses profile that may use the
/// server-count overflow-recovery path.
#[must_use]
pub fn is_openai_responses_portable_target_profile(provider_name: &str, model_name: &str) -> bool {
    provider_name == "openai_responses" && model_name == OPENAI_RESPONSES_PORTABLE_TARGET_MODEL
}

/// Returns whether automatic cache-aware V3 rotation has both a local exact portable proof
/// profile and a trusted route capability.
///
/// Model identity alone is insufficient: compatible/custom routes remain ineligible until their
/// adapter advertises a validated cache contract. Profiles that need
/// provider-side token measurement (currently OpenAI Responses) are reserved for the separately
/// bounded overflow-recovery path and cannot enter the idle automatic path.
#[must_use]
pub fn cache_aware_v3_automatic_supported(
    provider_name: &str,
    model_name: &str,
    capabilities: &sigil_kernel::ProviderContextCapabilities,
) -> bool {
    is_deepseek_v4_flash_portable_target_profile(provider_name, model_name)
        && capabilities.validate().is_ok()
        && !matches!(
            capabilities.cache_mode,
            sigil_kernel::CacheMode::Unknown | sigil_kernel::CacheMode::ObservedImplicitOrNone
        )
}

/// Requires the resolved DeepSeek transport to match the pinned portable-token profile.
///
/// The local tokenizer profile is only evidence for DeepSeek's default public routes and their
/// default request shaping. A proxy, route override, or alternate user/strict-tool policy can
/// change the wire request without changing the `CompletionRequest`, so those configurations are
/// deliberately unavailable for portable compaction rather than inheriting the default proof.
///
/// # Errors
///
/// Returns an error when the resolved DeepSeek configuration is unavailable or differs from the
/// pinned default transport profile.
pub fn require_default_deepseek_v4_flash_portable_transport(
    root_config: &sigil_kernel::RootConfig,
) -> Result<()> {
    let resolved = crate::resolve_deepseek_config(root_config)
        .context("could not resolve DeepSeek transport for portable compaction")?;
    require_deepseek_v4_flash_portable_transport_config(&resolved)
}

/// Requires the exact persisted V2 connection to match the pinned DeepSeek portable profile.
///
/// # Errors
///
/// Returns an error when the connection is missing, uses another protocol, or changes any
/// provider material covered by the exact local token proof.
pub fn require_deepseek_v4_flash_portable_transport_for_model_ref(
    root_config: &sigil_kernel::RootConfig,
    model_ref: &sigil_kernel::ModelRef,
) -> Result<()> {
    let loaded = crate::provider_connections::load_provider_connections(root_config);
    let connection = loaded
        .connections
        .get(&model_ref.connection_id)
        .context("could not resolve exact DeepSeek transport for portable compaction")?;
    if connection.config.provider != crate::provider_connections::ProviderFamily::DeepSeek
        || connection.config.protocol != crate::provider_connections::ProviderProtocol::DeepSeek
    {
        bail!("exact portable target proof requires a DeepSeek connection");
    }
    let mut resolved: DeepSeekProviderConfig =
        crate::provider_factory::exact_connection_provider_config(&connection.config, None)?;
    resolved.model = model_ref.model_id.clone();
    require_deepseek_v4_flash_portable_transport_config(&resolved)
}

fn require_deepseek_v4_flash_portable_transport_config(
    resolved: &DeepSeekProviderConfig,
) -> Result<()> {
    let expected = DeepSeekProviderConfig::default_for_model(DEFAULT_DEEPSEEK_V4_FLASH_MODEL);
    let matches_pinned_transport = resolved.model == expected.model
        && resolved.base_url == expected.base_url
        && resolved.beta_base_url == expected.beta_base_url
        && resolved.anthropic_base_url == expected.anthropic_base_url
        && resolved.user_id_strategy == expected.user_id_strategy
        && resolved.strict_tools_mode == StrictToolsMode::Auto;
    if !matches_pinned_transport {
        bail!(
            "local exact portable target proof requires the resolved default DeepSeek V4 Flash transport; custom routes, user_id_strategy, and strict_tools_mode are unsupported"
        );
    }
    Ok(())
}

/// Returns the explicit output reservation required by an admitted portable target profile.
///
/// A value here only materializes an explicit target request. It does not imply local admission
/// or authorize provider I/O; the caller must still obtain that profile's own exact proof.
#[must_use]
pub fn portable_compaction_target_output_tokens(
    provider_name: &str,
    model_name: &str,
) -> Option<u32> {
    if is_deepseek_v4_flash_portable_target_profile(provider_name, model_name) {
        Some(deepseek_v4_flash_portable_target_output_tokens())
    } else if is_openai_responses_portable_target_profile(provider_name, model_name) {
        Some(OPENAI_RESPONSES_PORTABLE_TARGET_OUTPUT_TOKENS)
    } else {
        None
    }
}

/// Result of an exact local DeepSeek V4 Flash portable-target pressure assessment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepSeekV4FlashPortableTargetPressure {
    ExactFit {
        binding: TokenMeasurementBinding,
        proof: Box<RequestFitProof>,
    },
    ExceedsBudget {
        input_tokens: u64,
        budget: sigil_kernel::EffectiveTokenBudget,
    },
}

/// Classifies a frozen request against the local default DeepSeek V4 Flash target profile.
///
/// This only opens the checksum-pinned tokenizer already present under `cache_root`. It never
/// downloads an artifact or contacts a provider. An exceeded budget remains a normal local
/// pressure outcome rather than an invalid proof.
///
/// # Errors
///
/// Returns an error when the request is outside the admitted profile, lacks the explicit output
/// reservation, or the verified tokenizer is unavailable.
pub fn deepseek_v4_flash_portable_target_pressure(
    cache_root: &Path,
    frozen_request: &FrozenProviderRequestMaterial,
) -> Result<DeepSeekV4FlashPortableTargetPressure> {
    let request = frozen_request.request();
    if !is_deepseek_v4_flash_portable_target_profile(
        request.provider_name.as_str(),
        request.model_name.as_str(),
    ) {
        bail!("local exact portable target proof is unavailable for this provider/model");
    }
    if request.max_tokens != Some(deepseek_v4_flash_portable_target_output_tokens()) {
        bail!(
            "local exact portable target proof requires explicit max_tokens={}",
            deepseek_v4_flash_portable_target_output_tokens()
        );
    }
    let tokenizer_path = default_deepseek_v4_flash_tokenizer_cache_path(cache_root);
    let counter = DeepSeekV4FlashTokenCounter::from_official_tokenizer_path(&tokenizer_path)
        .with_context(|| {
            format!(
                "verified DeepSeek V4 tokenizer is unavailable at {}",
                tokenizer_path.display()
            )
        })?;
    match counter.default_portable_target_request_admission(frozen_request)? {
        DeepSeekV4FlashPortableTargetAdmission::ExactFit { binding, proof } => {
            Ok(DeepSeekV4FlashPortableTargetPressure::ExactFit { binding, proof })
        }
        DeepSeekV4FlashPortableTargetAdmission::ExceedsBudget {
            input_tokens,
            budget,
        } => Ok(DeepSeekV4FlashPortableTargetPressure::ExceedsBudget {
            input_tokens,
            budget,
        }),
    }
}

/// Proves a frozen request against the local default DeepSeek V4 Flash portable target profile.
///
/// This only opens the checksum-pinned tokenizer already present under `cache_root`. It never
/// downloads an artifact or contacts a provider. The returned binding and proof are both tied to
/// the supplied frozen request, including its explicit output reservation.
///
/// # Errors
///
/// Returns an error when the local verified tokenizer is unavailable or the frozen request cannot
/// satisfy the explicit default DeepSeek portable-compaction target budget.
pub fn deepseek_v4_flash_portable_target_proof(
    cache_root: &Path,
    frozen_request: &FrozenProviderRequestMaterial,
) -> Result<(TokenMeasurementBinding, RequestFitProof)> {
    match deepseek_v4_flash_portable_target_pressure(cache_root, frozen_request)? {
        DeepSeekV4FlashPortableTargetPressure::ExactFit { binding, proof } => Ok((binding, *proof)),
        DeepSeekV4FlashPortableTargetPressure::ExceedsBudget { .. } => {
            bail!("token evidence does not fit the effective request budget")
        }
    }
}

/// Builds the admitted local DeepSeek V4 Flash target material for portable compaction.
///
/// This only opens the checksum-pinned tokenizer already present under `cache_root`. It never
/// downloads an artifact, contacts a provider, or exposes a tokenizer setup action through the
/// compaction confirmation flow.
///
/// # Errors
///
/// Returns an error when the local verified tokenizer is unavailable or the frozen request cannot
/// satisfy the explicit default DeepSeek portable-compaction target budget.
pub fn deepseek_v4_flash_portable_target_material(
    cache_root: &Path,
    frozen_request: FrozenProviderRequestMaterial,
) -> Result<PortableTargetRequestMaterial> {
    let (binding, proof) = deepseek_v4_flash_portable_target_proof(cache_root, &frozen_request)?;
    Ok(PortableTargetRequestMaterial::new(
        frozen_request,
        binding,
        proof,
    ))
}

/// Builds portable target material only when separately frozen pre-activation material proves
/// that the checkpoint saves both the configured absolute and relative token minima.
///
/// Both requests are rendered through the same checksum-pinned tokenizer profile. The before
/// request remains process-local; only its fingerprint and exact token evidence become durable
/// if the caller later activates the checkpoint.
pub fn deepseek_v4_flash_portable_target_material_with_economics(
    cache_root: &Path,
    frozen_before_request: &FrozenProviderRequestMaterial,
    frozen_target_request: FrozenProviderRequestMaterial,
) -> Result<PortableTargetRequestMaterial> {
    let tokenizer_path = default_deepseek_v4_flash_tokenizer_cache_path(cache_root);
    let counter = DeepSeekV4FlashTokenCounter::from_official_tokenizer_path(&tokenizer_path)
        .with_context(|| {
            format!(
                "verified DeepSeek V4 tokenizer is unavailable at {}",
                tokenizer_path.display()
            )
        })?;
    let (binding, proof) =
        counter.exact_default_portable_target_request_fit(&frozen_target_request)?;
    let before_input = counter.exact_target_input_evidence(frozen_before_request)?;
    PortableTargetRequestMaterial::new(frozen_target_request, binding, proof)
        .with_portable_economics(frozen_before_request, before_input)
}

/// Builds the exact positive-savings candidate consumed immediately by RFC-0057 admission.
///
/// The attached economics record decides fit-required bypass and the 4K/5% or trusted-price
/// policy.
pub fn deepseek_v4_flash_portable_target_material_with_economics_v2_candidate(
    cache_root: &Path,
    frozen_before_request: &FrozenProviderRequestMaterial,
    frozen_target_request: FrozenProviderRequestMaterial,
) -> Result<PortableTargetRequestMaterial> {
    let tokenizer_path = default_deepseek_v4_flash_tokenizer_cache_path(cache_root);
    let counter = DeepSeekV4FlashTokenCounter::from_official_tokenizer_path(&tokenizer_path)
        .with_context(|| {
            format!(
                "verified DeepSeek V4 tokenizer is unavailable at {}",
                tokenizer_path.display()
            )
        })?;
    let (binding, proof) =
        counter.exact_default_portable_target_request_fit(&frozen_target_request)?;
    let before_input = counter.exact_target_input_evidence(frozen_before_request)?;
    PortableTargetRequestMaterial::new(frozen_target_request, binding, proof)
        .with_portable_economics_v2_candidate(frozen_before_request, before_input)
}

/// Installs the checksum-pinned tokenizer required by the admitted DeepSeek portable profile.
///
/// Callers must make the network destination and artifact purpose visible and obtain user intent
/// before invoking this explicit setup action. Normal compaction preview and apply never call it.
pub async fn install_default_deepseek_v4_flash_tokenizer(
    cache_root: &Path,
) -> Result<std::path::PathBuf> {
    let client = reqwest::Client::builder()
        .build()
        .context("failed to create tokenizer download client")?;
    download_default_deepseek_v4_flash_tokenizer(&client, cache_root).await
}

#[cfg(test)]
#[path = "tests/portable_compaction_tests.rs"]
mod tests;

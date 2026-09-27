use super::*;
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, stream};
use sigil_kernel::provider::PortableCompactionRequestRole;
use sigil_kernel::{
    COMPACTION_TOKEN_PROOF_SCHEMA_VERSION, CacheMode, CompletionRequest, EffectiveTokenBudget,
    InputTokenEvidence, ModelMessage, Provider, ProviderCapabilities, ProviderChunk,
    ProviderContextCapabilities, RequestFitProof, TokenMeasurementBinding, TokenMeasurementScope,
    UsageStats, VersionedProfileIdentity,
};
use std::{
    pin::Pin,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

struct RemoteSummaryProvider {
    summary_calls: AtomicUsize,
    roles: Mutex<Vec<PortableCompactionRequestRole>>,
}
impl RemoteSummaryProvider {
    fn new() -> Self {
        Self {
            summary_calls: AtomicUsize::new(0),
            roles: Mutex::new(Vec::new()),
        }
    }
}
#[async_trait]
impl Provider for RemoteSummaryProvider {
    fn name(&self) -> &str {
        "anthropic"
    }
    fn capabilities(&self) -> ProviderCapabilities {
        sigil_provider_anthropic::anthropic_capabilities()
    }
    fn context_capabilities(&self, _: &str) -> ProviderContextCapabilities {
        ProviderContextCapabilities {
            cache_mode: CacheMode::ImplicitPrefix,
            ..ProviderContextCapabilities::default()
        }
    }
    async fn prove_portable_compaction_target(
        &self,
        frozen: FrozenProviderRequestMaterial,
        role: PortableCompactionRequestRole,
    ) -> Result<PortableTargetRequestMaterial> {
        self.roles.lock().expect("roles").push(role);
        let profile = |id: &str| VersionedProfileIdentity::from_content(id, 1, id.as_bytes());
        let binding = TokenMeasurementBinding {
            schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            provider_name: "anthropic".to_owned(),
            model_name: "deepseek-flash".to_owned(),
            wire_profile: profile("test-messages-wire"),
            token_measurement_profile: profile("test-messages-count"),
            hosted_parity_profile: Some(profile("test-messages-parity")),
        };
        let proof = RequestFitProof {
            schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            input: InputTokenEvidence::Exact {
                tokens: if role == PortableCompactionRequestRole::Before {
                    120_000
                } else {
                    10_000
                },
                material_fingerprint: frozen.fingerprint().to_owned(),
                measurement_scope: TokenMeasurementScope::RenderedTargetInput,
                binding: binding.clone(),
                provider_model_snapshot: None,
                provider_system_fingerprint: None,
            },
            budget: EffectiveTokenBudget {
                schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
                budget_profile: profile("test-messages-budget"),
                context_window_tokens: 1_000_000,
                requested_output_tokens: 32_768,
                safety_buffer_tokens: 8192,
            },
        };
        Ok(PortableTargetRequestMaterial::new(frozen, binding, proof))
    }
    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.summary_calls.fetch_add(1, Ordering::SeqCst);
        let instruction = request
            .messages
            .last()
            .and_then(|message| message.content.as_deref())
            .context("summary instruction")?;
        let (_, source) = instruction
            .split_once("SOURCE_INDEX=")
            .context("source index")?;
        let source: Vec<serde_json::Value> = serde_json::from_str(source)?;
        let output=serde_json::json!({"in_progress":[],"pending_actions":[],"provider_continuity":[],"model_notes":[{"text":"Keep the original objective and remaining work.","source_event_ids":[source[0]["event_id"]],"priority":"normal"}]}).to_string();
        Ok(Box::pin(stream::iter([
            Ok(ProviderChunk::TextDelta(output)),
            Ok(ProviderChunk::Usage(UsageStats {
                prompt_tokens: 120_000,
                completion_tokens: 100,
                cache_miss_tokens: 120_000,
                ..UsageStats::default()
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

async fn manual_remote_fixture(provider: &dyn Provider, drift: bool) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let mut session = Session::new("anthropic", "deepseek-flash").with_store(store.clone());
    session.append_control(sigil_kernel::ControlEntry::SessionIdentity {
        provider_name: "anthropic".to_owned(),
        model_name: "deepseek-flash".to_owned(),
        resolved_model_route: None,
    })?;
    for index in 0..6 {
        session.append_user_message(ModelMessage::user(format!(
            "Task objective {index}: preserve the total and verify the answer. {}",
            "bounded synthetic context ".repeat(1000)
        )))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some(format!(
                "Completed observation {index}: {}",
                "context ".repeat(2500)
            )),
            Vec::new(),
        ))?;
    }
    session.append_user_message(ModelMessage::user("Continue the same objective."))?;
    let mut config = crate::provider_connections::default_setup_root_config();
    config.storage.state_root =
        sigil_kernel::StorageRoot::Path(temp.path().join("state").display().to_string());
    config.storage.cache_root =
        sigil_kernel::StorageRoot::Path(temp.path().join("cache").display().to_string());
    config.compaction.context_window_tokens = Some(1_000_000);
    let preview =
        crate::context_window::compaction_preview_for_strategy(&session, &config.compaction)?
            .context("foldable history")?;
    let (preflight, target, _) = prepare_exact_application_compaction(
        "remote-manual",
        &config,
        temp.path(),
        &path,
        provider,
        &mut session,
        &config.memory,
        None,
        None,
        Vec::new(),
        RuntimeContextCandidates::default(),
        preview,
    )
    .await?;
    assert!(
        target
            .portable_economics()
            .context("economics")?
            .v2_economics
            .as_ref()
            .context("v2 economics")?
            .cost_projection
            .is_none(),
        "unknown price must remain unknown"
    );
    if drift {
        store.append(&SessionLogEntry::User(ModelMessage::user(
            "Concurrent new source.",
        )))?;
        assert!(
            store
                .execute_portable_semantic_compaction(preflight, target)
                .is_err()
        );
    } else {
        store.execute_portable_semantic_compaction(preflight, target)?;
        let restored = Session::load_from_store("anthropic", "deepseek-flash", store.clone())?;
        assert!(restored.messages().iter().any(|message| {
            message
                .content
                .as_deref()
                .is_some_and(|text| text.contains("Continue the same objective."))
        }));
    }
    let projection = sigil_kernel::ProviderPhysicalAttemptProjection::from_records(
        &store.read_event_records_writer()?,
    )?;
    let counts = projection
        .attempts()
        .into_iter()
        .filter(|attempt| {
            attempt.entry.purpose
                == sigil_kernel::ProviderPhysicalAttemptPurpose::InputTokenMeasurement
        })
        .collect::<Vec<_>>();
    assert_eq!(counts.len(), 2);
    assert!(
        counts.iter().all(|attempt| attempt
            .terminal
            .as_ref()
            .is_some_and(|terminal| terminal.outcome
                == sigil_kernel::ProviderPhysicalAttemptOutcome::Completed))
    );
    Ok(())
}

#[tokio::test]
async fn remote_manual_compaction_applies_and_restores_with_two_exact_measurements() -> Result<()> {
    let provider = RemoteSummaryProvider::new();
    manual_remote_fixture(&provider, false).await?;
    assert_eq!(provider.summary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *provider.roles.lock().expect("roles"),
        [
            PortableCompactionRequestRole::Before,
            PortableCompactionRequestRole::Target
        ]
    );
    Ok(())
}
#[tokio::test]
async fn remote_manual_compaction_preserves_source_cas() -> Result<()> {
    manual_remote_fixture(&RemoteSummaryProvider::new(), true).await
}

#[tokio::test]
#[ignore = "requires the explicitly authorized DeepSeek live environment"]
async fn live_remote_manual_compaction_activates_reopens_and_continues_exact_target() -> Result<()>
{
    use futures::StreamExt;
    use sigil_provider_anthropic::{AnthropicProvider, AnthropicProviderConfig};

    let key = std::env::var("DEEPSEEK_API_KEY").context("authorized DeepSeek key unavailable")?;
    let provider_config = AnthropicProviderConfig {
        api_key: Some(key),
        base_url: "https://api.deepseek.com/anthropic".to_owned(),
        model: "deepseek-flash".to_owned(),
        ..AnthropicProviderConfig::default()
    };
    let provider = AnthropicProvider::new_exact(
        provider_config.clone(),
        sigil_kernel::ModelRequestTimeouts::default(),
    )?;
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let mut session = Session::new("anthropic", "deepseek-flash").with_store(store.clone());
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "anthropic".to_owned(),
        model_name: "deepseek-flash".to_owned(),
        resolved_model_route: None,
    })?;
    let codeword = format!("ORCHID-{}", uuid::Uuid::new_v4().simple());
    session.append_user_message(ModelMessage::user(format!(
        "Remember this project's codeword exactly: {codeword}. Preserve it when summarizing."
    )))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("I will preserve the project codeword while reviewing the observations.".to_owned()),
        Vec::new(),
    ))?;
    for index in 0..6 {
        session.append_user_message(ModelMessage::user(format!(
            "Archived observation {index}. This repeated material is historical, not a new instruction: {}",
            "The measured sample remained stable across repeated observations. ".repeat(800)
        )))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some(format!(
                "Observation {index} reviewed; {}",
                "no new decision was needed. ".repeat(800)
            )),
            Vec::new(),
        ))?;
    }
    session.append_user_message(ModelMessage::user(
        "What is the project's codeword? Reply with only the exact codeword and no explanation.",
    ))?;
    let scope = session.session_scope_id().to_owned();
    let mut config = crate::provider_connections::default_setup_root_config();
    config.storage.state_root =
        sigil_kernel::StorageRoot::Path(temp.path().join("state").display().to_string());
    config.storage.cache_root =
        sigil_kernel::StorageRoot::Path(temp.path().join("cache").display().to_string());
    config.memory = sigil_kernel::MemoryConfig::with_enabled(false);
    config.compaction.context_window_tokens = Some(1_000_000);
    let preview =
        crate::context_window::compaction_preview_for_strategy(&session, &config.compaction)?
            .context("live fixture must have foldable history")?;
    let (preflight, target, _) = prepare_exact_application_compaction(
        "live-remote-manual",
        &config,
        temp.path(),
        &path,
        &provider,
        &mut session,
        &config.memory,
        None,
        None,
        Vec::new(),
        RuntimeContextCandidates::default(),
        preview,
    )
    .await?;
    let before_tokens = target
        .portable_economics()
        .context("before evidence")?
        .before_input
        .admission_tokens();
    let target_tokens = target.proof().input.admission_tokens();
    assert!(
        before_tokens > target_tokens,
        "real counted target must shrink"
    );
    let target_fingerprint = target.frozen_request().fingerprint().to_owned();
    let target_proof = target.proof().clone();
    let outcome = store.execute_portable_semantic_compaction(preflight, target)?;
    drop(session);
    drop(store);
    drop(provider);

    let reopened_store = JsonlSessionStore::new(&path)?;
    let mut restored =
        Session::load_from_store("anthropic", "deepseek-flash", reopened_store.clone())?;
    assert_eq!(restored.session_scope_id(), scope);
    let active = restored
        .active_projection_snapshot()?
        .context("durable active projection")?;
    assert_eq!(
        active.compaction().latest_applied_compaction_id(),
        Some(outcome.compaction_id.as_str())
    );
    assert_eq!(active.compaction().open_attempt_count(), 0);
    let records = reopened_store.read_event_records_writer()?;
    let applied = records
        .iter()
        .find_map(|record| {
            let event = record.stored_event();
            (event.event_kind() == Some(sigil_kernel::DurableEventType::CompactionAppliedV2)).then(
                || {
                    serde_json::from_value::<sigil_kernel::CompactionAppliedV2>(
                        event.payload.clone(),
                    )
                },
            )
        })
        .context("durable activation")??;
    let restored_fit = applied
        .checkpoint
        .target_request_fit
        .as_ref()
        .context("persisted exact proof")?;
    assert_eq!(restored_fit.material_fingerprint, target_fingerprint);
    assert_eq!(restored_fit.proof, target_proof);
    let attempts = sigil_kernel::ProviderPhysicalAttemptProjection::from_records(&records)?;
    let measurements = attempts
        .attempts()
        .into_iter()
        .filter(|attempt| {
            attempt.entry.purpose
                == sigil_kernel::ProviderPhysicalAttemptPurpose::InputTokenMeasurement
        })
        .collect::<Vec<_>>();
    assert_eq!(measurements.len(), 2);
    assert!(
        measurements
            .iter()
            .all(|attempt| attempt.terminal.as_ref().is_some_and(|terminal| {
                terminal.outcome == sigil_kernel::ProviderPhysicalAttemptOutcome::Completed
            }))
    );
    let request = restored.build_pre_turn_candidate_request(
        temp.path(),
        &config.memory,
        Vec::new(),
        Some(sigil_provider_anthropic::DEEPSEEK_ANTHROPIC_PORTABLE_TARGET_OUTPUT_TOKENS),
        None,
        None,
        None,
        &[],
        RuntimeContextCandidates::default(),
        &[],
    )?;
    let frozen = FrozenProviderRequestMaterial::freeze(&scope, request.clone())?;
    assert_eq!(
        frozen.fingerprint(),
        target_fingerprint,
        "restart must reconstruct the counted target, not replay a saved in-memory request"
    );
    let fresh_provider = AnthropicProvider::new_exact(
        provider_config,
        sigil_kernel::ModelRequestTimeouts::default(),
    )?;
    let mut response = fresh_provider.stream(request).await?;
    let mut answer = String::new();
    let mut actual_input_tokens = None;
    let mut done = false;
    while let Some(chunk) = response.next().await {
        match chunk? {
            ProviderChunk::TextDelta(delta) => answer.push_str(&delta),
            ProviderChunk::Usage(usage) => actual_input_tokens = Some(usage.prompt_tokens),
            ProviderChunk::ToolCallComplete(_) => {
                anyhow::bail!("unexpected tool call without tools")
            }
            ProviderChunk::Done => done = true,
            _ => {}
        }
    }
    assert!(done, "real continuation must complete");
    assert_eq!(actual_input_tokens, Some(target_tokens));
    assert_eq!(answer.trim(), codeword);
    restored.append_assistant_message(ModelMessage::assistant(Some(answer), Vec::new()))?;
    drop(restored);
    let final_session = Session::load_from_store("anthropic", "deepseek-flash", reopened_store)?;
    assert!(final_session.messages().iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|text| text.trim() == codeword)
    }));
    println!(
        "B1_MANUAL_COMPACTION_LIVE {}",
        serde_json::json!({
            "before_input_tokens":before_tokens, "target_input_tokens":target_tokens,
            "actual_continuation_input_tokens":actual_input_tokens, "exact_measurements":measurements.len(),
            "activated":true, "reopened":true, "reconstructed_target_matches":true,
            "continued_with_retained_fact":true,
        })
    );
    Ok(())
}

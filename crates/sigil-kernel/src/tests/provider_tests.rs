use std::pin::Pin;

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, stream};

use crate::{MessageRole, ModelMessage, ReasoningEffort, ToolCall};

use super::{
    CacheMode, CacheTokenCountV1, CacheUsageCapabilities, CacheUsageV1, CompletionRequest,
    ModelPricingSnapshotV1, Provider, ProviderCapabilities, ProviderChunk,
    ProviderContextCapabilities, ReasoningStreamSupport, SessionStats, ToolCallCompletionIdPolicy,
    ToolCallStreamAccumulator, UsageStats,
};

#[test]
fn session_stats_track_latest_prompt_tokens_separately_from_totals() {
    let mut stats = SessionStats::default();
    stats.apply_usage(&UsageStats {
        prompt_tokens: 120,
        completion_tokens: 10,
        cache_hit_tokens: 80,
        cache_miss_tokens: 40,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
        cache_usage: None,
        pricing_snapshot: None,
    });
    stats.apply_usage(&UsageStats {
        prompt_tokens: 42,
        completion_tokens: 5,
        cache_hit_tokens: 21,
        cache_miss_tokens: 21,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
        cache_usage: None,
        pricing_snapshot: None,
    });

    assert_eq!(stats.prompt_tokens, 162);
    assert_eq!(stats.last_prompt_tokens, 42);
}

#[test]
fn cache_usage_preserves_unknown_write_and_trusted_pricing_evidence() -> Result<()> {
    let usage = UsageStats {
        prompt_tokens: 100,
        completion_tokens: 20,
        cache_hit_tokens: 80,
        cache_miss_tokens: 20,
        cache_usage: Some(CacheUsageV1 {
            schema_version: CacheUsageV1::SCHEMA_VERSION,
            read: Some(CacheTokenCountV1::provider_reported(80)),
            write: None,
            uncached: Some(CacheTokenCountV1::provider_reported(20)),
            local_layout_mutation: Some(crate::CacheLayoutMutationKind::ConversationTailAppended),
            provider_miss_without_local_mutation: false,
        }),
        ..UsageStats::default()
    };
    let snapshot = ModelPricingSnapshotV1 {
        schema_version: ModelPricingSnapshotV1::SCHEMA_VERSION,
        snapshot_id: "test-model-usd-2026-07-28".to_owned(),
        currency: "USD".to_owned(),
        unit_tokens: 1_000_000,
        cache_read_per_unit: 0.1,
        cache_write_per_unit: None,
        uncached_input_per_unit: 1.0,
        output_per_unit: 2.0,
        source: "https://example.invalid/trusted-pricing".to_owned(),
        verified_at: "2026-07-28".to_owned(),
    };

    let priced = snapshot.apply_to_usage(usage)?;
    let cache = priced.cache_usage.as_ref().expect("cache usage");
    assert!(cache.write.is_none(), "unknown write must not become zero");
    assert_eq!(priced.input_cost, 0.000028);
    assert_eq!(priced.output_cost, 0.00004);
    assert_eq!(priced.cache_savings, 0.000072);
    assert_eq!(
        priced
            .pricing_snapshot
            .as_ref()
            .map(|snapshot| snapshot.snapshot_id.as_str()),
        Some("test-model-usd-2026-07-28")
    );
    Ok(())
}

#[test]
fn provider_context_capability_validation_rejects_impossible_shapes() {
    let mut missing_limit = ProviderContextCapabilities {
        cache_mode: CacheMode::ExplicitBreakpoints,
        ..ProviderContextCapabilities::default()
    };
    assert!(missing_limit.validate().is_err());

    missing_limit.explicit_breakpoint_limit = Some(4);
    assert!(missing_limit.validate().is_ok());

    let mut unknown_with_limit = ProviderContextCapabilities::unknown();
    unknown_with_limit.explicit_breakpoint_limit = Some(1);
    assert!(unknown_with_limit.validate().is_err());
}

#[test]
fn pricing_does_not_treat_reported_write_as_free_when_write_price_is_unknown() -> Result<()> {
    let usage = UsageStats {
        prompt_tokens: 100,
        input_cost: 7.0,
        cache_usage: Some(CacheUsageV1 {
            schema_version: CacheUsageV1::SCHEMA_VERSION,
            read: Some(CacheTokenCountV1::provider_reported(40)),
            write: Some(CacheTokenCountV1::provider_reported(40)),
            uncached: Some(CacheTokenCountV1::provider_reported(20)),
            local_layout_mutation: None,
            provider_miss_without_local_mutation: false,
        }),
        ..UsageStats::default()
    };
    let snapshot = ModelPricingSnapshotV1 {
        schema_version: ModelPricingSnapshotV1::SCHEMA_VERSION,
        snapshot_id: "test-model-no-write-price".to_owned(),
        currency: "USD".to_owned(),
        unit_tokens: 1_000_000,
        cache_read_per_unit: 0.1,
        cache_write_per_unit: None,
        uncached_input_per_unit: 1.0,
        output_per_unit: 2.0,
        source: "bundled-test".to_owned(),
        verified_at: "2026-07-28".to_owned(),
    };

    let priced = snapshot.apply_to_usage(usage)?;
    assert_eq!(priced.input_cost, 7.0);
    assert!(priced.pricing_snapshot.is_some());
    Ok(())
}

#[test]
fn cache_usage_rejects_known_categories_larger_than_provider_prompt_total() {
    let usage = CacheUsageV1 {
        schema_version: CacheUsageV1::SCHEMA_VERSION,
        read: Some(CacheTokenCountV1::provider_reported(40)),
        write: Some(CacheTokenCountV1::provider_reported(40)),
        uncached: Some(CacheTokenCountV1::provider_reported(30)),
        local_layout_mutation: None,
        provider_miss_without_local_mutation: false,
    };

    let error = usage
        .validate_for_prompt_tokens(100)
        .expect_err("known cache categories must fit the provider prompt total");
    assert!(error.to_string().contains("exceed provider prompt tokens"));
}

#[test]
fn identical_local_layout_with_uncached_input_records_a_narrow_provider_miss_diagnostic() {
    let mut usage = CacheUsageV1 {
        schema_version: CacheUsageV1::SCHEMA_VERSION,
        read: Some(CacheTokenCountV1::provider_reported(0)),
        write: None,
        uncached: Some(CacheTokenCountV1::provider_reported(100)),
        local_layout_mutation: None,
        provider_miss_without_local_mutation: false,
    };

    usage.observe_local_layout(crate::CacheLayoutMutationKind::Identical);
    assert!(usage.provider_miss_without_local_mutation);

    let mut stats = SessionStats::default();
    stats.apply_usage(&UsageStats {
        prompt_tokens: 100,
        cache_miss_tokens: 100,
        cache_usage: Some(usage),
        ..UsageStats::default()
    });
    assert!(stats.last_provider_miss_without_local_mutation);
}

#[test]
fn a_local_history_rewrite_never_blames_an_uncached_request_on_the_provider() {
    let mut usage = CacheUsageV1 {
        schema_version: CacheUsageV1::SCHEMA_VERSION,
        read: Some(CacheTokenCountV1::provider_reported(0)),
        write: None,
        uncached: Some(CacheTokenCountV1::provider_reported(100)),
        local_layout_mutation: None,
        provider_miss_without_local_mutation: false,
    };

    usage.observe_local_layout(crate::CacheLayoutMutationKind::ConversationHistoryRewritten);
    assert!(!usage.provider_miss_without_local_mutation);
}

#[test]
fn reasoning_stream_support_tracks_surface_semantics() {
    assert_eq!(ReasoningStreamSupport::Unsupported.as_str(), "unsupported");
    assert_eq!(ReasoningStreamSupport::Passthrough.as_str(), "passthrough");
    assert_eq!(ReasoningStreamSupport::Native.as_str(), "native");
    assert!(!ReasoningStreamSupport::Unsupported.can_surface());
    assert!(ReasoningStreamSupport::Passthrough.can_surface());
    assert!(ReasoningStreamSupport::Native.can_surface());
}

#[test]
fn tool_call_stream_accumulator_emits_start_args_and_complete() {
    let mut accumulator = ToolCallStreamAccumulator::new();
    let mut chunks = Vec::new();

    accumulator.append_delta(
        &mut chunks,
        0,
        Some("call-1".to_owned()),
        Some("read_file".to_owned()),
        Some("{\"path\"".to_owned()),
    );
    accumulator.append_delta(
        &mut chunks,
        0,
        None,
        None,
        Some(":\"src/lib.rs\"}".to_owned()),
    );
    accumulator.complete_open_calls(&mut chunks, ToolCallCompletionIdPolicy::RequireProviderId);

    assert!(matches!(
        &chunks[0],
        ProviderChunk::ToolCallStart { id, name } if id == "call-1" && name == "read_file"
    ));
    assert!(matches!(
        &chunks[1],
        ProviderChunk::ToolCallArgsDelta { id, delta } if id == "call-1" && delta == "{\"path\""
    ));
    assert!(matches!(
        &chunks[2],
        ProviderChunk::ToolCallArgsDelta { id, delta } if id == "call-1" && delta == ":\"src/lib.rs\"}"
    ));
    assert!(matches!(
        &chunks[3],
        ProviderChunk::ToolCallComplete(call)
            if call.id == "call-1"
                && call.name == "read_file"
                && call.args_json == "{\"path\":\"src/lib.rs\"}"
    ));
}

#[test]
fn tool_call_stream_accumulator_respects_completion_id_policy() {
    let mut accumulator = ToolCallStreamAccumulator::new();
    let mut chunks = Vec::new();

    accumulator.append_delta(
        &mut chunks,
        2,
        None,
        Some("echo".to_owned()),
        Some("{}".to_owned()),
    );
    accumulator.complete_open_calls(&mut chunks, ToolCallCompletionIdPolicy::RequireProviderId);
    assert_eq!(chunks.len(), 2);

    accumulator.complete_open_calls(&mut chunks, ToolCallCompletionIdPolicy::SynthesizeFromIndex);
    assert!(matches!(
        chunks.last(),
        Some(ProviderChunk::ToolCallComplete(call))
            if call.id == "call-2" && call.name == "echo" && call.args_json == "{}"
    ));
}

#[test]
fn tool_call_stream_accumulator_keeps_event_id_when_provider_id_arrives_late() {
    let mut accumulator = ToolCallStreamAccumulator::new();
    let mut chunks = Vec::new();

    accumulator.append_delta(
        &mut chunks,
        0,
        None,
        Some("echo".to_owned()),
        Some("{\"value\"".to_owned()),
    );
    accumulator.append_delta(
        &mut chunks,
        0,
        Some("provider-call-1".to_owned()),
        None,
        Some(":1}".to_owned()),
    );
    accumulator.complete_open_calls(&mut chunks, ToolCallCompletionIdPolicy::RequireProviderId);

    assert!(matches!(
        &chunks[0],
        ProviderChunk::ToolCallStart { id, name } if id == "call-0" && name == "echo"
    ));
    assert!(matches!(
        &chunks[1],
        ProviderChunk::ToolCallArgsDelta { id, delta } if id == "call-0" && delta == "{\"value\""
    ));
    assert!(matches!(
        &chunks[2],
        ProviderChunk::ToolCallArgsDelta { id, delta } if id == "call-0" && delta == ":1}"
    ));
    assert!(matches!(
        &chunks[3],
        ProviderChunk::ToolCallComplete(call)
            if call.id == "call-0" && call.name == "echo" && call.args_json == "{\"value\":1}"
    ));
}

#[test]
fn tool_call_stream_accumulator_caps_sequential_completed_call_count() {
    let mut accumulator = ToolCallStreamAccumulator::new();
    let mut chunks = Vec::new();

    for index in 0..crate::MAX_PROVIDER_TURN_TOOL_CALLS {
        accumulator.append_delta(
            &mut chunks,
            index,
            Some(format!("call-{index}")),
            Some("echo".to_owned()),
            Some("{}".to_owned()),
        );
        accumulator.complete_open_calls(&mut chunks, ToolCallCompletionIdPolicy::RequireProviderId);
    }
    accumulator.append_delta(
        &mut chunks,
        crate::MAX_PROVIDER_TURN_TOOL_CALLS,
        Some("call-overflow".to_owned()),
        Some("echo".to_owned()),
        Some("{}".to_owned()),
    );

    assert!(matches!(
        chunks.last(),
        Some(ProviderChunk::ToolCallStreamError(
            crate::SafePersistenceError::ToolCallStreamInvalid { .. }
        ))
    ));
    let chunk_count = chunks.len();
    accumulator.append_delta(
        &mut chunks,
        crate::MAX_PROVIDER_TURN_TOOL_CALLS + 1,
        Some("ignored-after-terminal".to_owned()),
        Some("echo".to_owned()),
        Some("{}".to_owned()),
    );
    assert_eq!(chunks.len(), chunk_count);
}

#[test]
fn tool_call_stream_accumulator_caps_monotonic_aggregate_across_completed_calls() {
    let mut accumulator = ToolCallStreamAccumulator::new();
    let mut chunks = Vec::new();
    let per_call = crate::MAX_STREAMED_TOOL_ARGS_BYTES;
    let completed_calls = crate::MAX_PROVIDER_TURN_TOOL_ARGS_BYTES / per_call;

    for index in 0..completed_calls {
        accumulator.append_delta(
            &mut chunks,
            index,
            Some(format!("aggregate-{index}")),
            Some("echo".to_owned()),
            Some("x".repeat(per_call)),
        );
        accumulator.complete_open_calls(&mut chunks, ToolCallCompletionIdPolicy::RequireProviderId);
    }
    accumulator.append_delta(
        &mut chunks,
        completed_calls,
        Some("aggregate-overflow".to_owned()),
        Some("echo".to_owned()),
        Some("x".to_owned()),
    );
    assert!(matches!(
        chunks.last(),
        Some(ProviderChunk::ToolCallStreamError(
            crate::SafePersistenceError::ToolArgsTooLarge {
                limit_bytes: crate::MAX_PROVIDER_TURN_TOOL_ARGS_BYTES,
                ..
            }
        ))
    ));

    accumulator.clear();
    let before = chunks.len();
    accumulator.append_delta(
        &mut chunks,
        0,
        Some("after-clear".to_owned()),
        Some("echo".to_owned()),
        Some("{}".to_owned()),
    );
    assert!(
        chunks.len() > before,
        "clear must release raw buffers and terminal latch"
    );
}

#[test]
fn provider_chunk_debug_redacts_raw_deltas_and_completed_arguments() {
    let secret = "known-tool-secret";
    for chunk in [
        ProviderChunk::TextDelta(secret.to_owned()),
        ProviderChunk::ReasoningDelta(secret.to_owned()),
        ProviderChunk::ToolCallArgsDelta {
            id: format!("authorization:{secret}"),
            delta: format!(r#"{{"token":"{secret}"}}"#),
        },
        ProviderChunk::ToolCallStart {
            id: format!("https://example.com/?token={secret}"),
            name: format!("secret-token-{secret}"),
        },
        ProviderChunk::ToolCallComplete(ToolCall {
            id: format!("authorization:{secret}"),
            name: format!("secret-token-{secret}"),
            args_json: format!(r#"{{"token":"{secret}"}}"#),
        }),
    ] {
        assert!(!format!("{chunk:?}").contains(secret));
    }
}

#[test]
fn tool_call_stream_accumulator_debug_never_exposes_buffered_identity_or_arguments() {
    let secret = "known-buffered-secret";
    let mut accumulator = ToolCallStreamAccumulator::new();
    let mut chunks = Vec::new();
    accumulator.append_delta(
        &mut chunks,
        0,
        Some("call-safe".to_owned()),
        Some("webfetch".to_owned()),
        Some(format!(r#"{{"token":"{secret}"}}"#)),
    );

    let debug = format!("{accumulator:?}");
    assert!(!debug.contains(secret));
    assert!(!debug.contains("call-safe"));
    assert!(!debug.contains("webfetch"));
    assert!(debug.contains("total_args_bytes"));
}

struct BoxedProviderFixture;

#[async_trait]
impl Provider for BoxedProviderFixture {
    fn name(&self) -> &str {
        "boxed"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: true,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Unsupported,
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
            tool_name_max_chars: 32,
        }
    }

    fn context_capabilities(&self, _model_name: &str) -> ProviderContextCapabilities {
        ProviderContextCapabilities::observed_implicit_or_none(CacheUsageCapabilities {
            read_tokens: true,
            write_tokens: false,
            miss_tokens: true,
        })
    }

    fn usage_pricing_snapshot(&self, _model_name: &str) -> Option<ModelPricingSnapshotV1> {
        Some(ModelPricingSnapshotV1 {
            schema_version: ModelPricingSnapshotV1::SCHEMA_VERSION,
            snapshot_id: "boxed-pricing".to_owned(),
            currency: "USD".to_owned(),
            unit_tokens: 1_000_000,
            cache_read_per_unit: 0.1,
            cache_write_per_unit: None,
            uncached_input_per_unit: 1.0,
            output_per_unit: 2.0,
            source: "https://example.invalid".to_owned(),
            verified_at: "2026-07-28".to_owned(),
        })
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("boxed-result".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[tokio::test]
async fn boxed_provider_delegates_name_capabilities_and_stream() -> Result<()> {
    let provider: Box<dyn Provider> = Box::new(BoxedProviderFixture);

    assert_eq!(provider.name(), "boxed");
    assert_eq!(provider.capabilities().tool_name_max_chars, 32);
    assert_eq!(
        provider.context_capabilities("model").cache_mode,
        CacheMode::ObservedImplicitOrNone
    );
    assert_eq!(
        provider
            .usage_pricing_snapshot("model")
            .expect("boxed pricing delegation")
            .snapshot_id,
        "boxed-pricing"
    );

    let chunks = futures::StreamExt::collect::<Vec<_>>(
        provider
            .stream(CompletionRequest {
                provider_name: "boxed".to_owned(),
                model_name: "model".to_owned(),
                messages: Vec::new(),
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
            })
            .await?,
    )
    .await;

    assert_eq!(chunks.len(), 2);
    assert!(matches!(
        chunks[0].as_ref().expect("first chunk should be ok"),
        ProviderChunk::TextDelta(delta) if delta == "boxed-result"
    ));
    assert!(matches!(
        chunks[1].as_ref().expect("second chunk should be ok"),
        ProviderChunk::Done
    ));
    Ok(())
}

#[test]
fn provider_helpers_expose_stable_strings_and_message_constructors() {
    assert_eq!(ReasoningEffort::Low.as_str(), "low");
    assert_eq!(ReasoningEffort::Medium.as_str(), "medium");
    assert_eq!(ReasoningEffort::High.as_str(), "high");
    assert_eq!(ReasoningEffort::Max.as_str(), "max");

    let system = ModelMessage::system("rules");
    let user = ModelMessage::user("hello");
    let assistant = ModelMessage::assistant(
        Some("working".to_owned()),
        vec![ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{}".to_owned(),
        }],
    );
    let tool = ModelMessage::tool("call-1", "ok");
    let blank = ModelMessage::new(MessageRole::Assistant, None);

    assert_eq!(system.role, MessageRole::System);
    assert_eq!(user.role, MessageRole::User);
    assert_eq!(assistant.role, MessageRole::Assistant);
    assert_eq!(assistant.tool_calls.len(), 1);
    assert_eq!(tool.role, MessageRole::Tool);
    assert_eq!(tool.tool_call_id.as_deref(), Some("call-1"));
    assert!(blank.id.parse::<uuid::Uuid>().is_ok());

    let usage = UsageStats::default();
    assert_eq!(usage.prompt_tokens, 0);
    assert_eq!(usage.completion_tokens, 0);
    assert!(usage.system_fingerprint.is_none());
}

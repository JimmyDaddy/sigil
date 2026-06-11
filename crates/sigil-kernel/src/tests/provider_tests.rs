use std::pin::Pin;

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, stream};

use crate::{MessageRole, ModelMessage, ReasoningEffort, ToolCall};

use super::{
    CompletionRequest, Provider, ProviderCapabilities, ProviderChunk, SessionStats, UsageStats,
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
    });

    assert_eq!(stats.prompt_tokens, 162);
    assert_eq!(stats.last_prompt_tokens, 42);
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
            supports_reasoning_stream: false,
            supports_tool_stream: false,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 32,
        }
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

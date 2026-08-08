use std::{
    collections::VecDeque,
    env,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use futures::StreamExt;
use sigil_kernel::{
    ModelRequestTimeouts, PROVIDER_ERROR_BODY_LIMIT_BYTES, Provider, ProviderChunk,
    ProviderRequestRejection, ReasoningStreamSupport, ToolAccess, ToolCall, ToolCategory,
    ToolPreviewCapability, ToolSpec, provider_rate_limit_from_error,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::{Duration, sleep, timeout},
};

use crate::{
    DeepSeekFimCompletionRequest, DeepSeekPrefixCompletionRequest, models::DeepSeekStreamEnvelope,
    request::build_chat_request, stream::test_support::parse_sse_frames,
};

use super::DeepSeekProvider;

fn deepseek_provider(config: crate::DeepSeekProviderConfig) -> Result<DeepSeekProvider> {
    crate::test_env::with_clean_provider_env(|| {
        DeepSeekProvider::new(config, ModelRequestTimeouts::default())
    })
}

#[test]
fn custom_route_does_not_inherit_exact_cache_or_trusted_pricing_from_model_name() -> Result<()> {
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: "https://proxy.example.invalid/v1".to_owned(),
        beta_base_url: "https://proxy.example.invalid/beta".to_owned(),
        anthropic_base_url: "https://proxy.example.invalid/anthropic".to_owned(),
        ..crate::DeepSeekProviderConfig::default()
    })?;

    assert!(!provider.capabilities().exact_prefix_cache);
    assert_eq!(
        provider
            .context_capabilities("deepseek-v4-flash")
            .cache_mode,
        sigil_kernel::CacheMode::ObservedImplicitOrNone
    );
    assert!(
        provider
            .usage_pricing_snapshot("deepseek-v4-flash")
            .is_none()
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires explicit real-provider opt-in, secret, and local cost admission"]
async fn real_provider_three_exact_prefix_turns_report_cache_hit_and_miss_usage() -> Result<()> {
    const REQUEST_COUNT: u64 = 3;
    const CONSERVATIVE_INPUT_TOKENS_PER_REQUEST: u64 = 64_000;
    const MAX_OUTPUT_TOKENS_PER_REQUEST: u64 = 32;
    const REQUIRED_FLAG: &str = "SIGIL_REAL_PROVIDER_CACHE_CONFORMANCE";
    const BUDGET_ENV: &str = "SIGIL_REAL_PROVIDER_MAX_COST_USD";

    if env::var(REQUIRED_FLAG).as_deref() != Ok("1") {
        anyhow::bail!(
            "{REQUIRED_FLAG}=1 is required before this test may contact the real provider"
        );
    }
    let api_key = env::var(crate::SIGIL_API_KEY_ENV)
        .context("SIGIL_API_KEY is required for real cache conformance")?;
    let max_cost_usd = env::var(BUDGET_ENV)
        .context("SIGIL_REAL_PROVIDER_MAX_COST_USD is required")?
        .parse::<f64>()
        .context("real-provider cache budget must be a decimal USD value")?;
    if !(max_cost_usd.is_finite() && 0.0 < max_cost_usd && max_cost_usd <= 1.0) {
        anyhow::bail!("real-provider cache budget must be greater than zero and at most $1.00");
    }
    let provider = DeepSeekProvider::new_exact(
        crate::DeepSeekProviderConfig {
            api_key: Some(api_key),
            ..crate::DeepSeekProviderConfig::default_for_model("deepseek-v4-flash")
        },
        ModelRequestTimeouts {
            request_timeout: Duration::from_secs(30),
            stream_idle_timeout: Duration::from_secs(30),
            stream_total_timeout: Some(Duration::from_secs(60)),
        },
    )?;
    let pricing = provider
        .usage_pricing_snapshot("deepseek-v4-flash")
        .context("real cache conformance requires a trusted pricing snapshot")?;
    let unit_tokens = pricing.unit_tokens as f64;
    let conservative_reservation_usd = REQUEST_COUNT as f64
        * ((CONSERVATIVE_INPUT_TOKENS_PER_REQUEST as f64 * pricing.uncached_input_per_unit
            / unit_tokens)
            + (MAX_OUTPUT_TOKENS_PER_REQUEST as f64 * pricing.output_per_unit / unit_tokens));
    if conservative_reservation_usd > max_cost_usd {
        anyhow::bail!(
            "real-provider cache conformance reserves ${conservative_reservation_usd:.6}, above the admitted ${max_cost_usd:.6}"
        );
    }
    let mut request = simple_chat_request("deepseek-v4-flash");
    request.messages = vec![
        sigil_kernel::ModelMessage::system(format!(
            "SIGIL RFC-0057 exact-prefix cache conformance fixture. {}",
            "stable-prefix-segment ".repeat(2_000)
        )),
        sigil_kernel::ModelMessage::user(
            "Reply with exactly CACHE-CONFORMANCE and no additional text.",
        ),
    ];
    request.temperature = Some(0.0);
    request.max_tokens = Some(MAX_OUTPUT_TOKENS_PER_REQUEST as u32);
    request.traffic_partition_key =
        Some("sigil:rfc-0057:deepseek-real-cache-conformance".to_owned());

    let mut usage = Vec::new();
    for _ in 0..REQUEST_COUNT {
        let mut stream = provider.stream(request.clone()).await?;
        let mut observed = None;
        while let Some(chunk) = stream.next().await {
            if let ProviderChunk::Usage(value) = chunk? {
                observed = Some(value);
            }
        }
        usage.push(observed.context("DeepSeek response omitted cache usage")?);
    }

    assert_eq!(usage.len(), REQUEST_COUNT as usize);
    assert!(usage.iter().all(|usage| {
        usage.prompt_tokens > 0
            && usage
                .cache_hit_tokens
                .saturating_add(usage.cache_miss_tokens)
                > 0
            && usage.cache_usage.is_some()
    }));
    assert!(
        usage[0].cache_miss_tokens > 0,
        "the cold request must report uncached input"
    );
    assert!(
        usage[1..].iter().any(|usage| usage.cache_hit_tokens > 0),
        "at least one repeated exact-prefix request must report a cache hit"
    );
    Ok(())
}

#[tokio::test]
async fn three_exact_prefix_requests_preserve_wire_prefix_and_map_hit_miss_usage() -> Result<()> {
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let response = |hit: u64, miss: u64| {
        http_response(
            200,
            "text/event-stream",
            &format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"ok\"}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":{},\"completion_tokens\":1,\"prompt_cache_hit_tokens\":{hit},\"prompt_cache_miss_tokens\":{miss}}}}}\n\ndata: [DONE]\n\n",
                hit + miss
            ),
        )
    };
    let responses = Arc::new(Mutex::new(VecDeque::from([
        response(0, 256),
        response(224, 32),
        response(224, 32),
    ])));
    let server = spawn_recording_server(Arc::clone(&requests), responses).await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server,
        api_key: Some("test-key".to_owned()),
        user_id_strategy: None,
        ..crate::DeepSeekProviderConfig::default_for_model("deepseek-v4-flash")
    })?;
    let mut request = simple_chat_request("deepseek-v4-flash");
    request.messages = vec![
        sigil_kernel::ModelMessage::system(format!(
            "stable-system-prefix:{}",
            "same-prefix ".repeat(200)
        )),
        sigil_kernel::ModelMessage::user("same active request"),
    ];
    request.max_tokens = Some(8);
    request.temperature = Some(0.0);

    let mut observed = Vec::new();
    for _ in 0..3 {
        let mut stream = provider.stream(request.clone()).await?;
        while let Some(chunk) = stream.next().await {
            if let ProviderChunk::Usage(usage) = chunk? {
                observed.push((usage.cache_hit_tokens, usage.cache_miss_tokens));
            }
        }
    }
    assert_eq!(observed, vec![(0, 256), (224, 32), (224, 32)]);

    let requests = requests.lock().expect("requests poisoned");
    assert_eq!(requests.len(), 3);
    let bodies = requests
        .iter()
        .map(|request| request.split("\r\n\r\n").nth(1).unwrap_or_default())
        .collect::<Vec<_>>();
    assert!(
        bodies.iter().all(|body| *body == bodies[0]),
        "same logical request must keep byte-identical DeepSeek wire content"
    );
    assert!(bodies[0].contains("stable-system-prefix"));
    Ok(())
}

#[test]
fn request_body_injects_reasoning_replay_into_matching_assistant_message() -> Result<()> {
    let assistant = sigil_kernel::ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{}".to_owned(),
        }],
    );
    let assistant_id = assistant.id.clone();
    let request = sigil_kernel::CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        messages: vec![assistant],
        tools: vec![ToolSpec {
            name: "read_file".to_owned(),
            description: "read".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }],
        temperature: None,
        max_tokens: None,
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: vec![sigil_kernel::ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "deepseek.reasoning_replay".to_owned(),
            message_id: Some(assistant_id),
            opaque_blob: serde_json::json!({"reasoning_content":"think"}),
        }],
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    };
    let body = build_chat_request(
        &request,
        None,
        crate::StrictToolsMode::Off,
        &crate::DeepSeekProviderQuirkProfile::default(),
    )?
    .body;
    let first = &body.messages[0];
    assert_eq!(first["reasoning_content"], "think");
    Ok(())
}

#[test]
fn sse_parser_ignores_comments_and_blanks() -> Result<()> {
    let frames = parse_sse_frames(":keepalive\n\ndata: {\"choices\":[]}\n\n")?;
    assert!(matches!(
        frames[0],
        crate::response::DeepSeekSseFrame::Comment
    ));
    assert!(matches!(
        frames[1],
        crate::response::DeepSeekSseFrame::Data(_)
    ));
    Ok(())
}

#[test]
fn reasoning_retry_and_mapper_helpers_cover_provider_side_branches() -> Result<()> {
    let state = crate::reasoning::DeepSeekReasoningReplayPayload {
        reasoning_content: "step by step".to_owned(),
    }
    .into_state();
    assert_eq!(
        state.state_kind,
        crate::reasoning::REASONING_REPLAY_STATE_KIND
    );
    assert_eq!(state.opaque_blob["reasoning_content"], "step by step");

    assert!(matches!(
        crate::retry::classify_status(401, ""),
        crate::errors::DeepSeekProviderError::Authentication(401)
    ));
    assert!(matches!(
        crate::retry::classify_status(402, ""),
        crate::errors::DeepSeekProviderError::Billing(402)
    ));
    assert!(matches!(
        crate::retry::classify_status(429, ""),
        crate::errors::DeepSeekProviderError::RateLimited
    ));
    assert!(matches!(
        crate::retry::classify_status(503, ""),
        crate::errors::DeepSeekProviderError::RetryableStatus(503)
    ));
    assert!(matches!(
        crate::retry::classify_status(400, "bad input"),
        crate::errors::DeepSeekProviderError::InvalidRequest(ref body) if body == "bad input"
    ));

    let mut mapper = crate::mapper::StreamMapper::new("deepseek-v4-flash");
    let envelope: DeepSeekStreamEnvelope = serde_json::from_value(serde_json::json!({
        "choices": [{
            "delta": {
                "content": "hello",
                "reasoning_content": "think",
                "tool_calls": [{
                    "index": 0,
                    "id": "call-1",
                    "function": {
                        "name": "read_file",
                        "arguments": "{\"path\":\"src/lib.rs\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 2,
            "prompt_cache_hit_tokens": 4,
            "prompt_cache_miss_tokens": 6
        },
        "system_fingerprint": "fp-1"
    }))?;

    let chunks = mapper.map_envelope(envelope)?;
    assert!(matches!(
        chunks.as_slice(),
        [
            ProviderChunk::Usage(_),
            ProviderChunk::TextDelta(text),
            ProviderChunk::ReasoningDelta(reasoning),
            ProviderChunk::ToolCallStart { id, name },
            ProviderChunk::ToolCallArgsDelta { id: args_id, delta },
            ProviderChunk::ToolCallComplete(call),
            ProviderChunk::ContinuationState(state)
        ] if text == "hello"
            && reasoning == "think"
            && id == "call-1"
            && name == "read_file"
            && args_id == "call-1"
            && delta == "{\"path\":\"src/lib.rs\"}"
            && call.id == "call-1"
            && call.name == "read_file"
            && call.args_json == "{\"path\":\"src/lib.rs\"}"
            && state.state_kind == crate::reasoning::REASONING_REPLAY_STATE_KIND
    ));

    let stop_envelope: DeepSeekStreamEnvelope = serde_json::from_value(serde_json::json!({
        "choices": [{
            "delta": { "reasoning_content": "done" },
            "finish_reason": "stop"
        }]
    }))?;
    let chunks = mapper.map_envelope(stop_envelope)?;
    assert!(
        matches!(chunks.as_slice(), [ProviderChunk::ReasoningDelta(reasoning)] if reasoning == "done")
    );
    Ok(())
}

#[test]
fn truncate_event_payload_adds_ellipsis_for_large_events() {
    let short = super::truncate_event_payload("short");
    assert_eq!(short, "short");

    let long = super::truncate_event_payload(&"x".repeat(300));
    assert!(long.ends_with("..."));
    assert!(long.len() < 300);
}

#[test]
fn provider_trait_methods_and_frame_helpers_cover_remaining_branches() -> Result<()> {
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: "http://primary.test".to_owned(),
        beta_base_url: "http://beta.test".to_owned(),
        anthropic_base_url: "http://anthropic.test".to_owned(),
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;

    assert_eq!(provider.name(), "deepseek");
    let capabilities = provider.capabilities();
    assert_eq!(
        capabilities.reasoning_stream,
        ReasoningStreamSupport::Native
    );
    assert!(capabilities.can_surface_reasoning_stream());
    assert!(capabilities.supports_reasoning_effort);
    assert!(capabilities.supports_tool_stream);
    assert!(capabilities.supports_infill_completion);
    assert!(capabilities.supports_system_fingerprint);
    assert!(capabilities.tool_name_max_chars > 0);
    assert_eq!(
        provider.base_url_for_endpoint(crate::endpoint::DeepSeekEndpointClass::AnthropicCompat),
        "http://anthropic.test"
    );

    let mut mapper = crate::mapper::StreamMapper::new("deepseek-v4-flash");
    let mut pending = VecDeque::new();
    assert!(!super::enqueue_chat_frame(
        &mut mapper,
        &mut pending,
        crate::response::DeepSeekSseFrame::Comment,
    )?);
    assert!(pending.is_empty());
    assert!(super::enqueue_chat_frame(
        &mut mapper,
        &mut pending,
        crate::response::DeepSeekSseFrame::Done,
    )?);
    assert!(matches!(pending.pop_front(), Some(ProviderChunk::Done)));

    let mut pending = VecDeque::new();
    assert!(!super::enqueue_completion_frame(
        &mut pending,
        crate::response::DeepSeekSseFrame::Blank,
    )?);
    assert!(super::enqueue_completion_frame(
        &mut pending,
        crate::response::DeepSeekSseFrame::Done,
    )?);
    assert!(matches!(pending.pop_front(), Some(ProviderChunk::Done)));

    let mut decoder = crate::stream::DeepSeekSseDecoder::default();
    let mut mapper = crate::mapper::StreamMapper::new("deepseek-v4-flash");
    let mut pending = VecDeque::new();
    decoder.push("data: [DONE]")?;
    assert!(super::enqueue_finished_chat_frames(
        &mut decoder,
        &mut mapper,
        &mut pending,
    )?);
    assert!(matches!(pending.pop_front(), Some(ProviderChunk::Done)));

    let mut decoder = crate::stream::DeepSeekSseDecoder::default();
    let mut pending = VecDeque::new();
    assert!(super::enqueue_completion_frames(
        &mut decoder,
        &mut pending,
        "data: [DONE]\n\ndata: {not-json}\n\n",
    )?);
    assert!(matches!(pending.pop_front(), Some(ProviderChunk::Done)));

    let mut decoder = crate::stream::DeepSeekSseDecoder::default();
    let mut pending = VecDeque::new();
    decoder.push("data: [DONE]")?;
    assert!(super::enqueue_finished_completion_frames(
        &mut decoder,
        &mut pending,
    )?);
    assert!(matches!(pending.pop_front(), Some(ProviderChunk::Done)));
    Ok(())
}

#[tokio::test]
async fn prefix_completion_rejects_unsupported_user_id_strategy() -> Result<()> {
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: "http://127.0.0.1:9".to_owned(),
        beta_base_url: "http://127.0.0.1:9".to_owned(),
        anthropic_base_url: "http://127.0.0.1:9".to_owned(),
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: Some("unsupported".to_owned()),
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;

    let error = match provider
        .stream_prefix_completion(DeepSeekPrefixCompletionRequest {
            model: None,
            prompt: "write code".to_owned(),
            assistant_prefix: "```rust\n".to_owned(),
            stop: Vec::new(),
            reasoning_effort: None,
            traffic_partition_key: Some("workspace-123".to_owned()),
        })
        .await
    {
        Ok(_) => panic!("unsupported user id strategies should fail before transport"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("unsupported user_id strategy"));
    Ok(())
}

#[tokio::test]
async fn provider_classifies_only_transport_connect_failures_as_pre_dispatch() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    drop(listener);
    let base_url = format!("http://{address}");
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: base_url.clone(),
        beta_base_url: base_url.clone(),
        anthropic_base_url: base_url,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;

    let error = match provider
        .stream(simple_chat_request("deepseek-v4-flash"))
        .await
    {
        Ok(_) => panic!("a refused connection should fail before a stream is established"),
        Err(error) => error,
    };

    assert_eq!(
        provider.classify_pre_generation_rejection(&error),
        Some(ProviderRequestRejection::ConnectFailedBeforeDispatch)
    );
    Ok(())
}

#[tokio::test]
async fn provider_surfaces_reasoning_400_without_a_second_send() -> Result<()> {
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![
        http_response(
            400,
            "application/json",
            r#"{"error":{"message":"missing reasoning_content"}}"#,
        ),
        http_response(
            200,
            "text/event-stream",
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        ),
    ])));
    let server = spawn_mock_server(Arc::clone(&responses)).await?;
    let config = crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server.clone(),
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    };
    let provider = deepseek_provider(config.clone())?;
    let request = sigil_kernel::CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: config.model.clone(),
        messages: vec![sigil_kernel::ModelMessage::user("hi")],
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: vec![sigil_kernel::ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "deepseek.reasoning_replay".to_owned(),
            message_id: None,
            opaque_blob: serde_json::json!({"reasoning_content":"think"}),
        }],
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    };
    let error = match provider.stream(request).await {
        Ok(_) => panic!("reasoning replay rejection should surface without an internal retry"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("deepseek invalid request"));
    assert!(
        error
            .chain()
            .any(|cause| cause.to_string().contains("missing reasoning_content"))
    );
    assert_eq!(responses.lock().expect("responses lock").len(), 1);
    Ok(())
}

#[tokio::test]
async fn provider_reports_missing_api_key_before_network() -> Result<()> {
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: "http://127.0.0.1:9".to_owned(),
        beta_base_url: "http://127.0.0.1:9".to_owned(),
        anthropic_base_url: "http://127.0.0.1:9".to_owned(),
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;

    let error = match provider
        .stream_fim_completion(DeepSeekFimCompletionRequest {
            model: None,
            prompt: "fn main() {".to_owned(),
            suffix: "}\n".to_owned(),
            max_tokens: Some(8),
            stop: Vec::new(),
        })
        .await
    {
        Ok(_) => panic!("missing api key should fail"),
        Err(error) => error,
    };

    let message = format!("{error:#}");
    assert!(message.contains("deepseek completion request failed"));
    assert!(message.contains("missing api key"));
    Ok(())
}

#[tokio::test]
async fn provider_yields_first_delta_before_stream_finishes() -> Result<()> {
    let server = spawn_slow_streaming_server().await?;
    let config = crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    };
    let provider = deepseek_provider(config.clone())?;
    let request = sigil_kernel::CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: config.model.clone(),
        messages: vec![sigil_kernel::ModelMessage::user("hi")],
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
    };
    let mut stream = provider.stream(request).await?;

    let first = timeout(Duration::from_millis(500), stream.next())
        .await
        .expect("first delta should arrive before the server closes the stream")
        .expect("stream should yield one chunk")?;

    assert!(matches!(first, ProviderChunk::TextDelta(text) if text == "hello"));
    Ok(())
}

#[tokio::test]
async fn provider_stream_ends_after_done_without_waiting_for_socket_close() -> Result<()> {
    let server = spawn_done_then_hanging_streaming_server().await?;
    let config = crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    };
    let provider = deepseek_provider(config.clone())?;
    let request = sigil_kernel::CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: config.model.clone(),
        messages: vec![sigil_kernel::ModelMessage::user("hi")],
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
    };
    let mut stream = provider.stream(request).await?;

    let first = timeout(Duration::from_millis(500), stream.next())
        .await
        .expect("first delta should arrive")
        .expect("stream should yield text")?;
    assert!(matches!(first, ProviderChunk::TextDelta(text) if text == "hello"));

    let done = timeout(Duration::from_millis(500), stream.next())
        .await
        .expect("done should arrive")
        .expect("stream should yield done")?;
    assert!(matches!(done, ProviderChunk::Done));

    let finished = timeout(Duration::from_millis(500), stream.next())
        .await
        .expect("stream should end after done without waiting for socket close");
    assert!(finished.is_none());
    Ok(())
}

#[tokio::test]
async fn provider_surfaces_invalid_chat_and_completion_events() -> Result<()> {
    let chat_server = spawn_mock_server(Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        &format!("data: {{{}}}\n\n", "x".repeat(300)),
    )]))))
    .await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: chat_server.clone(),
        beta_base_url: chat_server.clone(),
        anthropic_base_url: chat_server.clone(),
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;
    let request = sigil_kernel::CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        messages: vec![sigil_kernel::ModelMessage::user("hi")],
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
    };
    let error = provider
        .stream(request)
        .await?
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .next()
        .expect("stream should yield one error")
        .expect_err("invalid chat event should fail");
    assert!(error.to_string().contains("invalid DeepSeek event"));
    assert!(error.to_string().contains("..."));

    let completion_server =
        spawn_mock_server(Arc::new(Mutex::new(VecDeque::from(vec![http_response(
            200,
            "text/event-stream",
            "data: {not-json}\n\n",
        )]))))
        .await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: completion_server.clone(),
        beta_base_url: completion_server.clone(),
        anthropic_base_url: completion_server.clone(),
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;
    let error = provider
        .stream_fim_completion(DeepSeekFimCompletionRequest {
            model: None,
            prompt: "fn main() {\n".to_owned(),
            suffix: "\n}\n".to_owned(),
            max_tokens: Some(8),
            stop: Vec::new(),
        })
        .await?
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .next()
        .expect("stream should yield one error")
        .expect_err("invalid completion event should fail");
    assert!(
        error
            .to_string()
            .contains("invalid DeepSeek completion event")
    );
    Ok(())
}

#[tokio::test]
async fn prefix_completion_uses_beta_chat_path() -> Result<()> {
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"delta\":{\"content\":\"prefixed\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )])));
    let server = spawn_recording_server(Arc::clone(&requests), Arc::clone(&responses)).await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server.clone(),
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;

    let chunks = provider
        .stream_prefix_completion(DeepSeekPrefixCompletionRequest {
            model: None,
            prompt: "write code".to_owned(),
            assistant_prefix: "```rust\n".to_owned(),
            stop: vec!["```".to_owned()],
            reasoning_effort: None,
            traffic_partition_key: None,
        })
        .await?
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    assert!(
        chunks
            .iter()
            .any(|chunk| matches!(chunk, ProviderChunk::TextDelta(text) if text == "prefixed"))
    );
    let raw_request = requests
        .lock()
        .expect("requests poisoned")
        .pop_front()
        .expect("expected recorded prefix request");
    assert!(raw_request.contains("POST /chat/completions"));
    assert!(raw_request.contains("\"prefix\":true"));
    Ok(())
}

#[tokio::test]
async fn fim_completion_uses_completions_path() -> Result<()> {
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"text\":\"middle\",\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3,\"prompt_cache_hit_tokens\":2,\"prompt_cache_miss_tokens\":5},\"system_fingerprint\":\"fp-fim\"}\n\ndata: [DONE]\n\n",
    )])));
    let server = spawn_recording_server(Arc::clone(&requests), Arc::clone(&responses)).await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server.clone(),
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;

    let chunks = provider
        .stream_fim_completion(DeepSeekFimCompletionRequest {
            model: None,
            prompt: "fn main() {\n".to_owned(),
            suffix: "\n}\n".to_owned(),
            max_tokens: Some(32),
            stop: Vec::new(),
        })
        .await?
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    assert!(
        chunks
            .iter()
            .any(|chunk| matches!(chunk, ProviderChunk::TextDelta(text) if text == "middle"))
    );
    assert!(matches!(
        chunks.as_slice(),
        [
            ProviderChunk::TextDelta(text),
            ProviderChunk::Usage(usage),
            ProviderChunk::Done
        ] if text == "middle"
            && usage.prompt_tokens == 7
            && usage.completion_tokens == 3
            && usage.cache_hit_tokens == 2
            && usage.cache_miss_tokens == 5
            && usage.system_fingerprint.as_deref() == Some("fp-fim")
    ));
    let raw_request = requests
        .lock()
        .expect("requests poisoned")
        .pop_front()
        .expect("expected recorded fim request");
    assert!(raw_request.contains("POST /completions"));
    assert!(raw_request.contains("\"suffix\":\"\\n}\\n\""));
    Ok(())
}

#[tokio::test]
async fn fim_completion_yields_first_delta_before_stream_finishes() -> Result<()> {
    let server = spawn_slow_completion_streaming_server().await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;
    let mut stream = provider
        .stream_fim_completion(DeepSeekFimCompletionRequest {
            model: None,
            prompt: "fn main() {\n".to_owned(),
            suffix: "\n}\n".to_owned(),
            max_tokens: Some(32),
            stop: Vec::new(),
        })
        .await?;

    let first = timeout(Duration::from_millis(500), stream.next())
        .await
        .expect("first completion delta should arrive before the server closes the stream")
        .expect("stream should yield one chunk")?;

    assert!(matches!(first, ProviderChunk::TextDelta(text) if text == "middle"));
    Ok(())
}

#[tokio::test]
async fn provider_surfaces_rate_limited_status_without_a_second_send() -> Result<()> {
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![
        http_response_with_headers(
            429,
            "application/json",
            r#"{"error":{"message":"slow down"}}"#,
            "Retry-After: 60\r\n",
        ),
        http_response(
            200,
            "text/event-stream",
            "data: {\"choices\":[{\"delta\":{\"content\":\"after-retry\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        ),
    ])));
    let server = spawn_mock_server(Arc::clone(&responses)).await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server.clone(),
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;

    let error = match provider
        .stream(simple_chat_request("deepseek-v4-flash"))
        .await
    {
        Ok(_) => panic!("rate limit should surface without an internal retry"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("deepseek rate limited"));
    assert_eq!(provider.classify_pre_generation_rejection(&error), None);
    assert_eq!(
        provider_rate_limit_from_error(&error).and_then(|error| error.retry_after_ms()),
        Some(60_000)
    );
    assert_eq!(responses.lock().expect("responses lock").len(), 1);
    Ok(())
}

#[tokio::test]
async fn provider_returns_the_first_reasoning_rejection() -> Result<()> {
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![
        http_response(
            400,
            "application/json",
            r#"{"error":{"message":"missing reasoning_content"}}"#,
        ),
        http_response(
            400,
            "application/json",
            r#"{"error":{"message":"missing reasoning_content again"}}"#,
        ),
    ])));
    let server = spawn_mock_server(Arc::clone(&responses)).await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server.clone(),
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;

    let error = match provider
        .stream(chat_request_with_reasoning_state("deepseek-v4-flash"))
        .await
    {
        Ok(_) => panic!("second 400 should surface"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("deepseek invalid request"));
    assert!(
        error
            .chain()
            .any(|cause| cause.to_string().contains("missing reasoning_content"))
    );
    assert_eq!(responses.lock().expect("responses lock").len(), 1);
    Ok(())
}

#[tokio::test]
async fn provider_bounds_and_redacts_non_success_response_body() -> Result<()> {
    let api_key = "sk-provider-secret";
    let body = format!("token=visible {api_key} {}", "x".repeat(32 * 1024));
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        400,
        "text/plain",
        &body,
    )])));
    let server = spawn_mock_server(responses).await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some(api_key.to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;

    let error = match provider
        .stream(simple_chat_request("deepseek-v4-flash"))
        .await
    {
        Ok(_) => panic!("400 response should fail"),
        Err(error) => error,
    };
    let root = error.root_cause().to_string();

    assert!(!root.contains(api_key));
    assert!(!root.contains("visible"));
    assert!(root.contains("[redacted]"));
    assert!(root.len() <= PROVIDER_ERROR_BODY_LIMIT_BYTES + 64);
    Ok(())
}

#[tokio::test]
async fn provider_times_out_while_reading_non_success_response_body() -> Result<()> {
    let server = spawn_hanging_error_body_server().await?;
    let provider = crate::test_env::with_clean_provider_env(|| {
        DeepSeekProvider::new(
            crate::DeepSeekProviderConfig {
                base_url: server.clone(),
                beta_base_url: server.clone(),
                anthropic_base_url: server,
                model: "deepseek-v4-flash".to_owned(),
                fim_model: "deepseek-v4-pro".to_owned(),
                api_key: Some("test".to_owned()),
                user_id_strategy: None,
                strict_tools_mode: crate::StrictToolsMode::Auto,
            },
            ModelRequestTimeouts {
                request_timeout: Duration::from_millis(200),
                stream_idle_timeout: Duration::from_secs(1),
                stream_total_timeout: None,
            },
        )
    })?;

    let error = match provider
        .stream(simple_chat_request("deepseek-v4-flash"))
        .await
    {
        Ok(_) => panic!("hanging error body should time out"),
        Err(error) => error,
    };
    let chain = format!("{error:#}");

    assert!(chain.contains("failed to read deepseek error response body"));
    assert!(chain.contains("model deepseek-v4-flash"));
    assert!(chain.contains("status 400"));
    assert!(chain.contains("provider error response body timed out after 200 ms"));
    Ok(())
}

#[tokio::test]
async fn provider_emits_done_when_chat_stream_ends_without_done_frame() -> Result<()> {
    let server = spawn_chat_stream_without_done_server().await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;

    let chunks = provider
        .stream(simple_chat_request("deepseek-v4-flash"))
        .await?
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;

    assert!(matches!(
        chunks.as_slice(),
        [ProviderChunk::TextDelta(text), ProviderChunk::Done] if text == "tail"
    ));
    Ok(())
}

#[tokio::test]
async fn provider_surfaces_invalid_utf8_chat_chunks() -> Result<()> {
    let server = spawn_invalid_utf8_streaming_server().await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;
    let mut stream = provider
        .stream(simple_chat_request("deepseek-v4-flash"))
        .await?;

    let error = stream
        .next()
        .await
        .expect("stream should yield one error")
        .expect_err("invalid utf-8 should fail");

    assert!(
        error
            .chain()
            .any(|cause| cause.to_string().to_lowercase().contains("utf-8"))
    );
    Ok(())
}

#[tokio::test]
async fn provider_decodes_multibyte_chat_text_split_across_http_chunks() -> Result<()> {
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"é你🙂\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";
    let server = spawn_chunked_streaming_server(multibyte_split_chunks(body)).await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;

    let chunks = provider
        .stream(simple_chat_request("deepseek-v4-flash"))
        .await?
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;

    assert!(matches!(
        chunks.as_slice(),
        [ProviderChunk::TextDelta(text), ProviderChunk::Done] if text == "é你🙂"
    ));
    Ok(())
}

#[tokio::test]
async fn fim_completion_surfaces_invalid_utf8_chunks() -> Result<()> {
    let server = spawn_invalid_utf8_completion_streaming_server().await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;
    let mut stream = provider
        .stream_fim_completion(DeepSeekFimCompletionRequest {
            model: None,
            prompt: "fn main() {\n".to_owned(),
            suffix: "\n}\n".to_owned(),
            max_tokens: Some(32),
            stop: Vec::new(),
        })
        .await?;

    let error = stream
        .next()
        .await
        .expect("stream should yield one error")
        .expect_err("invalid utf-8 should fail");

    assert!(
        error
            .chain()
            .any(|cause| cause.to_string().to_lowercase().contains("utf-8"))
    );
    Ok(())
}

#[tokio::test]
async fn fim_completion_decodes_multibyte_text_split_across_http_chunks() -> Result<()> {
    let body =
        "data: {\"choices\":[{\"text\":\"é你🙂\",\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";
    let server = spawn_chunked_streaming_server(multibyte_split_chunks(body)).await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;

    let chunks = provider
        .stream_fim_completion(DeepSeekFimCompletionRequest {
            model: None,
            prompt: "fn main() {\n".to_owned(),
            suffix: "\n}\n".to_owned(),
            max_tokens: Some(32),
            stop: Vec::new(),
        })
        .await?
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;

    assert!(matches!(
        chunks.as_slice(),
        [ProviderChunk::TextDelta(text), ProviderChunk::Done] if text == "é你🙂"
    ));
    Ok(())
}

#[tokio::test]
async fn provider_surfaces_invalid_chat_event_payloads() -> Result<()> {
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        "data: {not-json}\n\n",
    )])));
    let server = spawn_mock_server(Arc::clone(&responses)).await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;
    let mut stream = provider
        .stream(simple_chat_request("deepseek-v4-flash"))
        .await?;

    let error = stream
        .next()
        .await
        .expect("stream should yield one error")
        .expect_err("invalid JSON should fail");

    assert!(error.to_string().contains("invalid DeepSeek event"));
    Ok(())
}

#[tokio::test]
async fn fim_completion_emits_done_when_stream_ends_without_done_frame() -> Result<()> {
    let server = spawn_completion_stream_without_done_server().await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;

    let chunks = provider
        .stream_fim_completion(DeepSeekFimCompletionRequest {
            model: None,
            prompt: "fn main() {\n".to_owned(),
            suffix: "\n}\n".to_owned(),
            max_tokens: Some(32),
            stop: Vec::new(),
        })
        .await?
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;

    assert!(matches!(
        chunks.as_slice(),
        [ProviderChunk::TextDelta(text), ProviderChunk::Done] if text == "tail-middle"
    ));
    Ok(())
}

#[tokio::test]
async fn provider_surfaces_chat_and_completion_body_read_errors() -> Result<()> {
    let chat_server = spawn_malformed_chunked_streaming_server().await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: chat_server.clone(),
        beta_base_url: chat_server.clone(),
        anthropic_base_url: chat_server,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;
    let mut stream = provider
        .stream(simple_chat_request("deepseek-v4-flash"))
        .await?;
    let error = stream
        .next()
        .await
        .expect("chat stream should yield one error")
        .expect_err("malformed chunked body should fail");
    assert!(
        error
            .chain()
            .any(|cause| cause.to_string().contains("failed to read response chunk"))
    );

    let completion_server = spawn_malformed_chunked_streaming_server().await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: completion_server.clone(),
        beta_base_url: completion_server.clone(),
        anthropic_base_url: completion_server,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;
    let mut stream = provider
        .stream_fim_completion(DeepSeekFimCompletionRequest {
            model: None,
            prompt: "fn main() {\n".to_owned(),
            suffix: "\n}\n".to_owned(),
            max_tokens: Some(32),
            stop: Vec::new(),
        })
        .await?;
    let error = stream
        .next()
        .await
        .expect("completion stream should yield one error")
        .expect_err("malformed chunked body should fail");
    assert!(error.chain().any(|cause| {
        cause
            .to_string()
            .contains("failed to read completion chunk")
    }));
    Ok(())
}

#[tokio::test]
async fn provider_surfaces_errors_from_unterminated_sse_frames() -> Result<()> {
    let chat_server = spawn_unterminated_sse_streaming_server("not-a-data-frame").await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: chat_server.clone(),
        beta_base_url: chat_server.clone(),
        anthropic_base_url: chat_server,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;
    let mut stream = provider
        .stream(simple_chat_request("deepseek-v4-flash"))
        .await?;
    let error = stream
        .next()
        .await
        .expect("chat stream should yield one error")
        .expect_err("invalid unterminated chat frame should fail");
    assert!(error.to_string().contains("invalid SSE chunk"));

    let completion_server = spawn_unterminated_sse_streaming_server("not-a-data-frame").await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: completion_server.clone(),
        beta_base_url: completion_server.clone(),
        anthropic_base_url: completion_server,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;
    let mut stream = provider
        .stream_fim_completion(DeepSeekFimCompletionRequest {
            model: None,
            prompt: "fn main() {\n".to_owned(),
            suffix: "\n}\n".to_owned(),
            max_tokens: Some(32),
            stop: Vec::new(),
        })
        .await?;
    let error = stream
        .next()
        .await
        .expect("completion stream should yield one error")
        .expect_err("invalid unterminated completion frame should fail");
    assert!(error.to_string().contains("invalid SSE chunk"));
    Ok(())
}

#[tokio::test]
async fn fim_completion_surfaces_non_success_status() -> Result<()> {
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        500,
        "application/json",
        r#"{"error":{"message":"server broke"}}"#,
    )])));
    let server = spawn_mock_server(Arc::clone(&responses)).await?;
    let provider = deepseek_provider(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server,
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
    })?;

    let error = match provider
        .stream_fim_completion(DeepSeekFimCompletionRequest {
            model: None,
            prompt: "fn main() {\n".to_owned(),
            suffix: "\n}\n".to_owned(),
            max_tokens: Some(32),
            stop: Vec::new(),
        })
        .await
    {
        Ok(_) => panic!("non-success completion response should fail"),
        Err(error) => error,
    };

    assert!(
        error
            .to_string()
            .contains("deepseek retryable server error 500")
    );
    Ok(())
}

fn simple_chat_request(model_name: &str) -> sigil_kernel::CompletionRequest {
    sigil_kernel::CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: model_name.to_owned(),
        messages: vec![sigil_kernel::ModelMessage::user("hi")],
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
    }
}

fn chat_request_with_reasoning_state(model_name: &str) -> sigil_kernel::CompletionRequest {
    let mut request = simple_chat_request(model_name);
    request
        .continuation_states
        .push(sigil_kernel::ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "deepseek.reasoning_replay".to_owned(),
            message_id: None,
            opaque_blob: serde_json::json!({"reasoning_content":"think"}),
        });
    request
}

async fn spawn_mock_server(responses: Arc<Mutex<VecDeque<Vec<u8>>>>) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let responses = Arc::clone(&responses);
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 4096];
                let _ = socket.read(&mut buffer).await;
                let response = responses
                    .lock()
                    .expect("mock server poisoned")
                    .pop_front()
                    .unwrap_or_else(|| http_response(500, "text/plain", "missing fixture"));
                let _ = socket.write_all(&response).await;
            });
        }
    });
    Ok(format!("http://{}", address))
}

async fn spawn_recording_server(
    requests: Arc<Mutex<VecDeque<String>>>,
    responses: Arc<Mutex<VecDeque<Vec<u8>>>>,
) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let requests = Arc::clone(&requests);
            let responses = Arc::clone(&responses);
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 8192];
                let bytes = socket.read(&mut buffer).await.unwrap_or(0);
                requests
                    .lock()
                    .expect("requests poisoned")
                    .push_back(String::from_utf8_lossy(&buffer[..bytes]).to_string());
                let response = responses
                    .lock()
                    .expect("mock server poisoned")
                    .pop_front()
                    .unwrap_or_else(|| http_response(500, "text/plain", "missing fixture"));
                let _ = socket.write_all(&response).await;
            });
        }
    });
    Ok(format!("http://{}", address))
}

async fn spawn_slow_streaming_server() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = vec![0u8; 8192];
        let _ = socket.read(&mut buffer).await;
        let header =
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
        let first =
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n";
        let _ = socket.write_all(header.as_bytes()).await;
        let _ = socket.write_all(first.as_bytes()).await;
        let _ = socket.flush().await;
        sleep(Duration::from_secs(1)).await;
        let done = "data: [DONE]\n\n";
        let _ = socket.write_all(done.as_bytes()).await;
    });
    Ok(format!("http://{}", address))
}

async fn spawn_done_then_hanging_streaming_server() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = vec![0u8; 8192];
        let _ = socket.read(&mut buffer).await;
        let header =
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\n";
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";
        let _ = socket.write_all(header.as_bytes()).await;
        let _ = socket.write_all(body.as_bytes()).await;
        let _ = socket.flush().await;
        sleep(Duration::from_secs(5)).await;
    });
    Ok(format!("http://{}", address))
}

async fn spawn_slow_completion_streaming_server() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = vec![0u8; 8192];
        let _ = socket.read(&mut buffer).await;
        let header =
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
        let first = "data: {\"choices\":[{\"text\":\"middle\",\"finish_reason\":null}]}\n\n";
        let _ = socket.write_all(header.as_bytes()).await;
        let _ = socket.write_all(first.as_bytes()).await;
        let _ = socket.flush().await;
        sleep(Duration::from_secs(1)).await;
        let done = "data: [DONE]\n\n";
        let _ = socket.write_all(done.as_bytes()).await;
    });
    Ok(format!("http://{}", address))
}

async fn spawn_chat_stream_without_done_server() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = vec![0u8; 8192];
        let _ = socket.read(&mut buffer).await;
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"tail\"},\"finish_reason\":null}]}\n\n";
        let _ = socket.write_all(response.as_bytes()).await;
    });
    Ok(format!("http://{}", address))
}

async fn spawn_completion_stream_without_done_server() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = vec![0u8; 8192];
        let _ = socket.read(&mut buffer).await;
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"text\":\"tail-middle\",\"finish_reason\":null}]}\n\n";
        let _ = socket.write_all(response.as_bytes()).await;
    });
    Ok(format!("http://{}", address))
}

async fn spawn_invalid_utf8_streaming_server() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = vec![0u8; 8192];
        let _ = socket.read(&mut buffer).await;
        let header =
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
        let _ = socket.write_all(header.as_bytes()).await;
        let _ = socket.write_all(&[0xff, 0xfe, 0xfd]).await;
    });
    Ok(format!("http://{}", address))
}

async fn spawn_invalid_utf8_completion_streaming_server() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = vec![0u8; 8192];
        let _ = socket.read(&mut buffer).await;
        let header =
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
        let _ = socket.write_all(header.as_bytes()).await;
        let _ = socket.write_all(&[0xff, 0xfe, 0xfd]).await;
    });
    Ok(format!("http://{}", address))
}

async fn spawn_malformed_chunked_streaming_server() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = vec![0u8; 8192];
        let _ = socket.read(&mut buffer).await;
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nZ\r\nbroken\r\n";
        let _ = socket.write_all(response.as_bytes()).await;
    });
    Ok(format!("http://{}", address))
}

async fn spawn_chunked_streaming_server(chunks: Vec<Vec<u8>>) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = vec![0u8; 8192];
        let _ = socket.read(&mut buffer).await;
        let header = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
        if socket.write_all(header.as_bytes()).await.is_err() {
            return;
        }
        for chunk in chunks {
            let prefix = format!("{:x}\r\n", chunk.len());
            if socket.write_all(prefix.as_bytes()).await.is_err()
                || socket.write_all(&chunk).await.is_err()
                || socket.write_all(b"\r\n").await.is_err()
            {
                return;
            }
            let _ = socket.flush().await;
            tokio::task::yield_now().await;
        }
        let _ = socket.write_all(b"0\r\n\r\n").await;
    });
    Ok(format!("http://{}", address))
}

async fn spawn_hanging_error_body_server() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = vec![0u8; 8192];
        let _ = socket.read(&mut buffer).await;
        let header = "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
        let _ = socket.write_all(header.as_bytes()).await;
        let _ = socket.flush().await;
        sleep(Duration::from_secs(1)).await;
    });
    Ok(format!("http://{}", address))
}

async fn spawn_unterminated_sse_streaming_server(body: &'static str) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = vec![0u8; 8192];
        let _ = socket.read(&mut buffer).await;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });
    Ok(format!("http://{}", address))
}

fn http_response(status: u16, content_type: &str, body: &str) -> Vec<u8> {
    http_response_with_headers(status, content_type, body, "")
}

fn http_response_with_headers(
    status: u16,
    content_type: &str,
    body: &str,
    extra_headers: &str,
) -> Vec<u8> {
    let status_line = match status {
        200 => "HTTP/1.1 200 OK",
        400 => "HTTP/1.1 400 Bad Request",
        429 => "HTTP/1.1 429 Too Many Requests",
        _ => "HTTP/1.1 500 Internal Server Error",
    };
    format!(
        "{status_line}\r\nContent-Type: {content_type}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes()
}

fn multibyte_split_chunks(body: &str) -> Vec<Vec<u8>> {
    let accented = body.find('é').expect("fixture contains two-byte character");
    let chinese = body
        .find('你')
        .expect("fixture contains three-byte character");
    let emoji = body
        .find('🙂')
        .expect("fixture contains four-byte character");
    let boundaries = [accented + 1, chinese + 2, emoji + 3];
    let mut chunks = Vec::new();
    let mut start = 0usize;
    for end in boundaries.into_iter().chain([body.len()]) {
        chunks.push(body.as_bytes()[start..end].to_vec());
        start = end;
    }
    chunks
}

#[test]
fn default_max_output_tokens_is_the_canonical_v4_cap() -> Result<()> {
    // The run boundary resolves this cap onto runs without explicit output constraints so every
    // provider request is deterministic (the Messages wire requires max_tokens).
    let provider = deepseek_provider(crate::DeepSeekProviderConfig::default())?;
    assert_eq!(
        Provider::default_max_output_tokens(&provider, "deepseek-v4-flash"),
        Some(crate::DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_OUTPUT_TOKENS)
    );
    Ok(())
}

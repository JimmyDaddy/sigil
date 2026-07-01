use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use futures::StreamExt;
use sigil_kernel::{
    ModelRequestTimeouts, Provider, ProviderChunk, ReasoningStreamSupport, ToolAccess, ToolCall,
    ToolCategory, ToolPreviewCapability, ToolSpec,
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
    let _guard = crate::test_env::lock();
    DeepSeekProvider::new(config, ModelRequestTimeouts::default())
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
        "deepseek-v4-flash",
    )?);
    assert!(super::enqueue_completion_frame(
        &mut pending,
        crate::response::DeepSeekSseFrame::Done,
        "deepseek-v4-flash",
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
        "deepseek-v4-flash",
    )?);
    assert!(matches!(pending.pop_front(), Some(ProviderChunk::Done)));

    let mut decoder = crate::stream::DeepSeekSseDecoder::default();
    let mut pending = VecDeque::new();
    decoder.push("data: [DONE]")?;
    assert!(super::enqueue_finished_completion_frames(
        &mut decoder,
        &mut pending,
        "deepseek-v4-flash",
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
async fn provider_retries_400_reasoning_and_yields_chunks() -> Result<()> {
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
    };
    let chunks = provider
        .stream(request)
        .await?
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    assert!(
        chunks
            .iter()
            .any(|chunk| matches!(chunk, ProviderChunk::TextDelta(text) if text == "hello"))
    );
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
async fn provider_retries_rate_limited_status_once() -> Result<()> {
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![
        http_response(
            429,
            "application/json",
            r#"{"error":{"message":"slow down"}}"#,
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

    let chunks = provider
        .stream(simple_chat_request("deepseek-v4-flash"))
        .await?
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;

    assert!(
        chunks
            .iter()
            .any(|chunk| matches!(chunk, ProviderChunk::TextDelta(text) if text == "after-retry"))
    );
    Ok(())
}

#[tokio::test]
async fn provider_returns_invalid_request_after_reasoning_retry_is_exhausted() -> Result<()> {
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
            .any(|cause| cause.to_string().contains("reasoning_content again"))
    );
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
    let status_line = match status {
        200 => "HTTP/1.1 200 OK",
        400 => "HTTP/1.1 400 Bad Request",
        429 => "HTTP/1.1 429 Too Many Requests",
        _ => "HTTP/1.1 500 Internal Server Error",
    };
    format!(
        "{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes()
}

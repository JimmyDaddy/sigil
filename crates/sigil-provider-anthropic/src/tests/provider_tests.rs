use std::{
    collections::VecDeque,
    ffi::OsString,
    sync::{Arc, Mutex},
};

use futures::StreamExt;
use sigil_kernel::{
    CompactionCursor, CompletionRequest, DurableEventType, FrozenProviderRequestMaterial,
    HostedEvidence, HostedToolKind, HostedToolLimits, HostedToolRequest, ImageInputCapability,
    JsonlSessionStore, ModelMessage, ModelRequestTimeouts, Provider, ProviderChunk, Session,
    SessionLogEntry,
};

use super::*;
use crate::{
    AnthropicNativeCompactionOptions, SIGIL_ANTHROPIC_API_KEY_ENV, SIGIL_ANTHROPIC_BASE_URL_ENV,
    SIGIL_ANTHROPIC_MAX_TOKENS_ENV, SIGIL_ANTHROPIC_VERSION_ENV,
};

struct EnvScope {
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl EnvScope {
    fn clear() -> Self {
        let names = [
            SIGIL_ANTHROPIC_API_KEY_ENV,
            "ANTHROPIC_API_KEY",
            SIGIL_ANTHROPIC_BASE_URL_ENV,
            SIGIL_ANTHROPIC_VERSION_ENV,
            SIGIL_ANTHROPIC_MAX_TOKENS_ENV,
        ];
        let previous = names
            .into_iter()
            .map(|name| (name, std::env::var_os(name)))
            .collect::<Vec<_>>();
        for name in names {
            unsafe {
                std::env::remove_var(name);
            }
        }
        Self { previous }
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        for (name, value) in self.previous.drain(..) {
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }
}

fn new_anthropic_provider(config: AnthropicProviderConfig) -> anyhow::Result<AnthropicProvider> {
    AnthropicProvider::new(config, ModelRequestTimeouts::default())
}

#[test]
fn provider_constructs_without_api_key_and_declares_name() -> anyhow::Result<()> {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::clear();
    let provider = new_anthropic_provider(AnthropicProviderConfig {
        api_key: None,
        ..AnthropicProviderConfig::default()
    })?;

    assert_eq!(provider.name(), "anthropic");
    assert!(provider.capabilities().supports_tool_stream);
    assert_eq!(
        provider.image_input_capability("claude-sonnet-4-6"),
        ImageInputCapability::Supported
    );
    assert_eq!(
        provider.image_input_capability("claude-test"),
        ImageInputCapability::Unsupported
    );
    Ok(())
}

#[tokio::test]
async fn provider_rejects_stream_without_api_key_before_network() -> anyhow::Result<()> {
    let provider = {
        let _guard = crate::test_env::lock();
        let _scope = EnvScope::clear();
        new_anthropic_provider(AnthropicProviderConfig {
            api_key: None,
            ..AnthropicProviderConfig::default()
        })?
    };

    let error = match provider
        .stream(CompletionRequest {
            provider_name: "anthropic".to_owned(),
            model_name: "claude-test".to_owned(),
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
        .await
    {
        Ok(_) => panic!("missing key should fail"),
        Err(error) => error,
    };

    assert!(format!("{error:#}").contains("missing Anthropic API key"));
    Ok(())
}

#[tokio::test]
async fn provider_stream_surfaces_sse_events_and_headers() -> anyhow::Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
         data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n\
         data: {\"type\":\"message_delta\",\"delta\":{},\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}\n\n\
         data: {\"type\":\"message_stop\"}\n\n",
    )
    .await?;
    let provider = anthropic_provider(AnthropicProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        beta_headers: vec![" tools-2024 ".to_owned(), "cache-2024".to_owned()],
        ..AnthropicProviderConfig::default()
    })?;

    let chunks = provider
        .stream(test_request())
        .await?
        .collect::<Vec<_>>()
        .await;

    assert!(
        matches!(chunks[0].as_ref().expect("text"), ProviderChunk::TextDelta(text) if text == "hi")
    );
    assert!(
        matches!(chunks[1].as_ref().expect("usage"), ProviderChunk::Usage(usage) if usage.prompt_tokens == 2)
    );
    assert!(matches!(
        chunks[2].as_ref().expect("done"),
        ProviderChunk::Done
    ));
    let request = server.request_text().to_ascii_lowercase();
    assert!(request.contains("x-api-key: test-key"));
    assert!(request.contains("anthropic-version: 2023-06-01"));
    assert!(request.contains("anthropic-beta: tools-2024,cache-2024"));
    assert!(request.contains("\"model\":\"claude-test\""));
    Ok(())
}

#[tokio::test]
async fn provider_native_compact_uses_paused_beta_wire_and_preserves_raw_content()
-> anyhow::Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n\
         {\"id\":\"msg_compact_1\",\"stop_reason\":\"compaction\",\"content\":[{\"type\":\"compaction\",\"content\":\"opaque-summary\",\"extension\":{\"retain\":true}}]}",
    )
    .await?;
    let provider = anthropic_hosted_test_provider(AnthropicProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        beta_headers: vec!["compact-2026-01-12".to_owned()],
        ..AnthropicProviderConfig::default()
    })?;
    let mut request = test_request();
    request.model_name = "claude-sonnet-4-6".to_owned();

    let compacted = provider
        .compact(
            &request,
            &AnthropicNativeCompactionOptions {
                trigger_input_tokens: 50_000,
                instructions: None,
            },
        )
        .await?;

    assert_eq!(compacted.response_id, "msg_compact_1");
    assert_eq!(
        compacted.canonical_compacted_content_json(),
        Some(r#"[{"type":"compaction","content":"opaque-summary","extension":{"retain":true}}]"#)
    );
    let wire = server.request_text().to_ascii_lowercase();
    assert!(wire.contains("anthropic-beta: compact-2026-01-12"));
    assert_eq!(wire.matches("compact-2026-01-12").count(), 1);
    assert!(wire.contains("\"stream\":false"));
    assert!(wire.contains("\"context_management\":{\"edits\":[{\"pause_after_compaction\":true"));
    assert!(wire.contains("\"type\":\"compact_20260112\""));
    Ok(())
}

#[tokio::test]
async fn provider_native_compact_records_a_completed_durable_attempt_when_the_threshold_is_not_met()
-> anyhow::Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n\
         {\"id\":\"msg_normal_1\",\"stop_reason\":\"end_turn\",\"content\":[{\"type\":\"text\",\"text\":\"normal\"}]}",
    )
    .await?;
    let provider = anthropic_hosted_test_provider(AnthropicProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..AnthropicProviderConfig::default()
    })?;
    let mut request = test_request();
    request.model_name = "claude-sonnet-4-6".to_owned();
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("anthropic", "claude-sonnet-4-6").with_store(store.clone());
    let covered = store.append_session_entry_event(&SessionLogEntry::User(ModelMessage::user(
        "durable native compaction seed",
    )))?;
    let frozen = FrozenProviderRequestMaterial::freeze(session.session_scope_id(), request)?;

    let materialized = provider
        .compact_and_materialize_durable(
            &session,
            "native-compaction-run-1",
            frozen,
            CompactionCursor {
                session_id: session.session_scope_id().to_owned(),
                through_stream_sequence: covered.stream_sequence,
                through_event_id: covered.event_id,
            },
            AnthropicNativeCompactionOptions {
                trigger_input_tokens: 50_000,
                instructions: None,
            },
        )
        .await?;

    assert!(materialized.is_none());
    let records = JsonlSessionStore::read_event_records(store.path())?;
    assert_eq!(
        records
            .iter()
            .filter(|record| {
                record.stored_event().event_type
                    == DurableEventType::ProviderPhysicalAttemptStarted.as_str()
            })
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| {
                record.stored_event().event_type
                    == DurableEventType::ProviderPhysicalAttemptTerminal.as_str()
            })
            .count(),
        1
    );
    assert!(!records.iter().any(|record| {
        record.stored_event().event_type == DurableEventType::ProviderContinuationObserved.as_str()
            || record.stored_event().event_type
                == DurableEventType::ProviderContinuationCandidateRecorded.as_str()
    }));
    Ok(())
}

#[tokio::test]
async fn provider_stream_decodes_multibyte_text_split_across_http_chunks() -> anyhow::Result<()> {
    let body = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"é你🙂\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n";
    let server = TinySseServer::start_chunked_body(multibyte_split_chunks(body)).await?;
    let provider = anthropic_provider(AnthropicProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..AnthropicProviderConfig::default()
    })?;

    let chunks = provider
        .stream(test_request())
        .await?
        .collect::<Vec<_>>()
        .await;

    assert!(
        matches!(chunks[0].as_ref().expect("text"), ProviderChunk::TextDelta(text) if text == "é你🙂")
    );
    assert!(matches!(
        chunks[1].as_ref().expect("done"),
        ProviderChunk::Done
    ));
    Ok(())
}

#[tokio::test]
async fn provider_stream_emits_done_when_http_stream_ends_without_message_stop()
-> anyhow::Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
         data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"tail\"}}\n\n",
    )
    .await?;
    let provider = anthropic_provider(AnthropicProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..AnthropicProviderConfig::default()
    })?;

    let chunks = provider
        .stream(test_request())
        .await?
        .collect::<Vec<_>>()
        .await;

    assert!(
        matches!(chunks[0].as_ref().expect("text"), ProviderChunk::TextDelta(text) if text == "tail")
    );
    assert!(matches!(
        chunks[1].as_ref().expect("done"),
        ProviderChunk::Done
    ));
    Ok(())
}

#[tokio::test]
async fn provider_stream_reports_invalid_json_and_utf8_chunks() -> anyhow::Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\ndata: {not-json}\n\n",
    )
    .await?;
    let provider = anthropic_provider(AnthropicProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..AnthropicProviderConfig::default()
    })?;
    let mut stream = provider.stream(test_request()).await?;
    let error = stream
        .next()
        .await
        .expect("stream item")
        .expect_err("invalid json should fail");
    assert!(error.to_string().contains("invalid Anthropic stream JSON"));
    assert!(stream.next().await.is_none());

    let server = TinySseServer::start_bytes(
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\ndata: \xff\n\n",
    )
    .await?;
    let provider = anthropic_provider(AnthropicProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..AnthropicProviderConfig::default()
    })?;
    let mut stream = provider.stream(test_request()).await?;
    let error = stream
        .next()
        .await
        .expect("stream item")
        .expect_err("invalid utf8 should fail");
    assert!(
        error
            .to_string()
            .contains("invalid UTF-8 SSE byte sequence")
    );
    assert!(stream.next().await.is_none());
    Ok(())
}

#[tokio::test]
async fn provider_stream_reports_body_read_and_unterminated_frame_errors() -> anyhow::Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 64\r\n\r\n",
    )
    .await?;
    let provider = anthropic_provider(AnthropicProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..AnthropicProviderConfig::default()
    })?;
    let mut stream = provider.stream(test_request()).await?;
    let mut body_error = None;
    while let Some(item) = stream.next().await {
        if let Err(error) = item {
            body_error = Some(error);
            break;
        }
    }
    let body_error = body_error.expect("short body should fail");
    assert!(
        body_error
            .to_string()
            .contains("failed to read response chunk")
    );

    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\nevent: message",
    )
    .await?;
    let provider = anthropic_provider(AnthropicProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..AnthropicProviderConfig::default()
    })?;
    let mut stream = provider.stream(test_request()).await?;
    let error = stream
        .next()
        .await
        .expect("stream item")
        .expect_err("unterminated invalid frame should fail");
    assert!(error.to_string().contains("invalid Anthropic SSE chunk"));
    assert!(stream.next().await.is_none());
    Ok(())
}

#[tokio::test]
async fn provider_stream_maps_http_error_status_once() -> anyhow::Result<()> {
    let server = TinySseServer::start_sequence(vec![
        "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 7\r\n\r\nlimited".as_bytes(),
        "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 7\r\n\r\nlimited".as_bytes(),
    ])
    .await?;
    let provider = anthropic_provider(AnthropicProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..AnthropicProviderConfig::default()
    })?;

    let error = match provider.stream(test_request()).await {
        Ok(_) => panic!("429 should fail"),
        Err(error) => error,
    };

    assert_eq!(error.to_string(), "Anthropic request was rate limited");
    assert_eq!(server.request_count(), 1);
    Ok(())
}

#[tokio::test]
async fn provider_hosted_search_integrates_wire_and_stream_without_raw_chunk_debug()
-> anyhow::Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
         data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"server_tool_use\",\"id\":\"srvtoolu_1\",\"name\":\"web_search\",\"input\":{\"query\":\"private query\"}}}\n\n\
         data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
         data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"web_search_tool_result\",\"tool_use_id\":\"srvtoolu_1\",\"content\":[{\"type\":\"web_search_result\",\"url\":\"https://example.com/?token=secret\",\"title\":\"private title\",\"encrypted_content\":\"encrypted-secret\"}]}}\n\n\
         data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":4,\"output_tokens\":2,\"server_tool_use\":{\"web_search_requests\":1}}}\n\n\
         data: {\"type\":\"message_stop\"}\n\n",
    )
    .await?;
    let provider = anthropic_hosted_test_provider(AnthropicProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..AnthropicProviderConfig::default()
    })?;

    let chunks = provider
        .stream(hosted_test_request("claude-sonnet-4-6"))
        .await?
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<anyhow::Result<Vec<_>>>()?;
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        ProviderChunk::HostedEvidence {
            evidence: HostedEvidence::Source(source),
            ..
        } if source.raw_url() == "https://example.com/?token=secret"
    )));
    let debug = format!("{chunks:?}");
    for secret in [
        "private query",
        "token=secret",
        "private title",
        "encrypted-secret",
    ] {
        assert!(!debug.contains(secret));
    }
    let wire = server.request_text();
    assert!(wire.contains("\"type\":\"web_search_20250305\""));
    assert!(wire.contains("\"max_uses\":2"));
    Ok(())
}

#[tokio::test]
async fn provider_hosted_search_rejects_unsupported_model_before_network() -> anyhow::Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\ndata: {\"type\":\"message_stop\"}\n\n",
    )
    .await?;
    let provider = anthropic_hosted_test_provider(AnthropicProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..AnthropicProviderConfig::default()
    })?;
    let error = match provider.stream(hosted_test_request("claude-test")).await {
        Ok(_) => panic!("unsupported model should fail"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("unsupported for model claude-test")
    );
    assert_eq!(server.request_count(), 0);
    Ok(())
}

#[tokio::test]
async fn provider_hosted_search_rejects_unsupported_platform_before_network() -> anyhow::Result<()>
{
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\ndata: {\"type\":\"message_stop\"}\n\n",
    )
    .await?;
    let provider = anthropic_provider(AnthropicProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..AnthropicProviderConfig::default()
    })?;

    assert!(
        !provider
            .hosted_web_search_capability("claude-sonnet-4-6")
            .is_supported()
    );
    let error = match provider
        .stream(hosted_test_request("claude-sonnet-4-6"))
        .await
    {
        Ok(_) => panic!("unsupported platform should fail"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("unsupported for this compatible endpoint")
    );
    assert_eq!(server.request_count(), 0);
    Ok(())
}

#[tokio::test]
async fn provider_hosted_search_never_transparently_retries_post_send_status() -> anyhow::Result<()>
{
    let server = TinySseServer::start_sequence(vec![
        "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 4\r\n\r\nbusy".as_bytes(),
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\ndata: {\"type\":\"message_stop\"}\n\n".as_bytes(),
    ])
    .await?;
    let provider = anthropic_hosted_test_provider(AnthropicProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..AnthropicProviderConfig::default()
    })?;
    let error = match provider
        .stream(hosted_test_request("claude-sonnet-4-6"))
        .await
    {
        Ok(_) => panic!("hosted 500 should terminate the invocation"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("retryable status 500"));
    assert_eq!(server.request_count(), 1);
    Ok(())
}

#[test]
fn provider_private_helpers_cover_beta_header_edges() -> anyhow::Result<()> {
    assert!(super::beta_header(&[" ".to_owned()])?.is_none());
    let error = super::beta_header(&["bad\nheader".to_owned()])
        .expect_err("invalid beta header should fail");
    assert!(error.to_string().contains("invalid Anthropic beta header"));

    let mut mapper = crate::mapper::StreamMapper::new(None);
    let mut pending = VecDeque::new();
    let done = super::enqueue_decoded_frames(
        &mut mapper,
        &mut pending,
        vec![
            crate::stream::AnthropicSseFrame::Comment,
            crate::stream::AnthropicSseFrame::Blank,
        ],
    )?;
    assert!(!done);
    assert!(pending.is_empty());
    Ok(())
}

fn anthropic_provider(config: AnthropicProviderConfig) -> anyhow::Result<AnthropicProvider> {
    anthropic_provider_with_timeouts(config, ModelRequestTimeouts::default())
}

fn anthropic_hosted_test_provider(
    config: AnthropicProviderConfig,
) -> anyhow::Result<AnthropicProvider> {
    let mut provider = anthropic_provider(config)?;
    provider.hosted_platform = crate::hosted_search::AnthropicHostedPlatform::ClaudeApi;
    Ok(provider)
}

fn anthropic_provider_with_timeouts(
    config: AnthropicProviderConfig,
    timeouts: ModelRequestTimeouts,
) -> anyhow::Result<AnthropicProvider> {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::clear();
    AnthropicProvider::new(config, timeouts)
}

fn test_request() -> CompletionRequest {
    CompletionRequest {
        provider_name: "anthropic".to_owned(),
        model_name: "claude-test".to_owned(),
        messages: vec![ModelMessage::user("hello")],
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

fn hosted_test_request(model_name: &str) -> CompletionRequest {
    let mut request = test_request();
    request.model_name = model_name.to_owned();
    request.hosted_tools = vec![
        HostedToolRequest::new(
            "authorization-1",
            HostedToolKind::WebSearch,
            HostedToolLimits {
                max_uses: Some(2),
                allowed_domains: vec!["example.com".to_owned()],
                blocked_domains: Vec::new(),
            },
        )
        .expect("hosted request fixture should validate"),
    ];
    request
}

struct TinySseServer {
    address: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl TinySseServer {
    async fn start(response: &'static str) -> anyhow::Result<Self> {
        Self::start_bytes(response.as_bytes()).await
    }

    async fn start_bytes(response: &'static [u8]) -> anyhow::Result<Self> {
        Self::start_sequence(vec![response]).await
    }

    async fn start_sequence(responses: Vec<&'static [u8]>) -> anyhow::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let request_capture = Arc::new(Mutex::new(Vec::new()));
        let task_request_capture = Arc::clone(&request_capture);
        tokio::spawn(async move {
            for response in responses {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                loop {
                    let Ok(read) = tokio::time::timeout(
                        std::time::Duration::from_millis(100),
                        tokio::io::AsyncReadExt::read(&mut socket, &mut buffer),
                    )
                    .await
                    else {
                        break;
                    };
                    let Ok(read) = read else {
                        break;
                    };
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                {
                    let mut captured = task_request_capture
                        .lock()
                        .expect("request capture mutex should not be poisoned");
                    captured.push(request);
                }
                let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, response).await;
            }
        });
        Ok(Self {
            address,
            requests: request_capture,
        })
    }

    async fn start_chunked_body(chunks: Vec<Vec<u8>>) -> anyhow::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let request_capture = Arc::new(Mutex::new(Vec::new()));
        let task_request_capture = Arc::clone(&request_capture);
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            loop {
                let Ok(read) = tokio::io::AsyncReadExt::read(&mut socket, &mut buffer).await else {
                    return;
                };
                if read == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            task_request_capture
                .lock()
                .expect("request capture mutex should not be poisoned")
                .push(request);
            let header = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n";
            if tokio::io::AsyncWriteExt::write_all(&mut socket, header.as_bytes())
                .await
                .is_err()
            {
                return;
            }
            for chunk in chunks {
                let prefix = format!("{:x}\r\n", chunk.len());
                if tokio::io::AsyncWriteExt::write_all(&mut socket, prefix.as_bytes())
                    .await
                    .is_err()
                    || tokio::io::AsyncWriteExt::write_all(&mut socket, &chunk)
                        .await
                        .is_err()
                    || tokio::io::AsyncWriteExt::write_all(&mut socket, b"\r\n")
                        .await
                        .is_err()
                {
                    return;
                }
                let _ = tokio::io::AsyncWriteExt::flush(&mut socket).await;
                tokio::task::yield_now().await;
            }
            let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, b"0\r\n\r\n").await;
        });
        Ok(Self {
            address,
            requests: request_capture,
        })
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn request_text(&self) -> String {
        let requests = self
            .requests
            .lock()
            .expect("request capture mutex should not be poisoned");
        let request = requests.first().map(Vec::as_slice).unwrap_or_default();
        String::from_utf8_lossy(request).into_owned()
    }

    fn request_count(&self) -> usize {
        self.requests
            .lock()
            .expect("request capture mutex should not be poisoned")
            .len()
    }
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

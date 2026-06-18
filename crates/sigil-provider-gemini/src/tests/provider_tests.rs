use std::{
    collections::VecDeque,
    ffi::OsString,
    sync::{Arc, Mutex},
};

use futures::StreamExt;
use sigil_kernel::{CompletionRequest, ModelMessage, Provider, ProviderChunk};

use super::*;
use crate::{
    GEMINI_API_KEY_ENV, GOOGLE_API_KEY_ENV, SIGIL_GEMINI_API_KEY_ENV, SIGIL_GEMINI_BASE_URL_ENV,
    SIGIL_GEMINI_MODEL_ENV, SIGIL_GEMINI_REQUEST_TIMEOUT_SECS_ENV,
};

struct EnvScope {
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl EnvScope {
    fn clear() -> Self {
        let names = [
            SIGIL_GEMINI_API_KEY_ENV,
            GEMINI_API_KEY_ENV,
            GOOGLE_API_KEY_ENV,
            SIGIL_GEMINI_MODEL_ENV,
            SIGIL_GEMINI_BASE_URL_ENV,
            SIGIL_GEMINI_REQUEST_TIMEOUT_SECS_ENV,
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

#[test]
fn provider_constructs_without_api_key_and_declares_name() -> anyhow::Result<()> {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::clear();
    let provider = GeminiProvider::new(GeminiProviderConfig {
        api_key: None,
        ..GeminiProviderConfig::default()
    })?;

    assert_eq!(provider.name(), "gemini");
    assert!(provider.capabilities().supports_structured_output);
    Ok(())
}

#[tokio::test]
async fn provider_rejects_stream_without_api_key_before_network() -> anyhow::Result<()> {
    let provider = {
        let _guard = crate::test_env::lock();
        let _scope = EnvScope::clear();
        GeminiProvider::new(GeminiProviderConfig {
            api_key: None,
            ..GeminiProviderConfig::default()
        })?
    };

    let error = match provider
        .stream(CompletionRequest {
            provider_name: "gemini".to_owned(),
            model_name: "gemini-test".to_owned(),
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
        .await
    {
        Ok(_) => panic!("missing key should fail"),
        Err(error) => error,
    };

    assert!(format!("{error:#}").contains("missing Gemini API key"));
    Ok(())
}

#[tokio::test]
async fn provider_stream_surfaces_sse_events_and_query() -> anyhow::Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
         data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]}}],\"usageMetadata\":{\"promptTokenCount\":2,\"candidatesTokenCount\":1}}\n\n\
         data: [DONE]\n\n",
    )
    .await?;
    let provider = gemini_provider(GeminiProviderConfig {
        base_url: server.base_url(),
        model: "models/gemini-test".to_owned(),
        api_key: Some("test-key".to_owned()),
        ..GeminiProviderConfig::default()
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
    let request = server.request_text();
    assert!(request.starts_with("POST /models/gemini-test:streamGenerateContent?"));
    assert!(request.contains("alt=sse"));
    assert!(request.contains("key=test-key"));
    assert!(request.contains("\"role\":\"user\""));
    Ok(())
}

#[tokio::test]
async fn provider_stream_url_uses_request_model_name() -> anyhow::Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
         data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]}}]}\n\n\
         data: [DONE]\n\n",
    )
    .await?;
    let provider = gemini_provider(GeminiProviderConfig {
        base_url: server.base_url(),
        model: "gemini-config".to_owned(),
        api_key: Some("test-key".to_owned()),
        ..GeminiProviderConfig::default()
    })?;
    let mut request = test_request();
    request.model_name = "gemini-request".to_owned();

    let chunks = provider.stream(request).await?.collect::<Vec<_>>().await;

    assert!(
        matches!(chunks[0].as_ref().expect("text"), ProviderChunk::TextDelta(text) if text == "hi")
    );
    let raw_request = server.request_text();
    assert!(raw_request.starts_with("POST /models/gemini-request:streamGenerateContent?"));
    assert!(!raw_request.contains("gemini-config:streamGenerateContent"));
    Ok(())
}

#[tokio::test]
async fn provider_stream_url_falls_back_to_prefixed_config_model() -> anyhow::Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
         data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]}}]}\n\n\
         data: [DONE]\n\n",
    )
    .await?;
    let provider = gemini_provider(GeminiProviderConfig {
        base_url: server.base_url(),
        model: "models/gemini-config".to_owned(),
        api_key: Some("test-key".to_owned()),
        ..GeminiProviderConfig::default()
    })?;
    let mut request = test_request();
    request.model_name = " ".to_owned();

    let chunks = provider.stream(request).await?.collect::<Vec<_>>().await;

    assert!(
        matches!(chunks[0].as_ref().expect("text"), ProviderChunk::TextDelta(text) if text == "hi")
    );
    let raw_request = server.request_text();
    assert!(raw_request.starts_with("POST /models/gemini-config:streamGenerateContent?"));
    assert!(!raw_request.contains("/models/models/gemini-config"));
    Ok(())
}

#[tokio::test]
async fn provider_stream_emits_done_when_http_stream_ends_without_done_frame() -> anyhow::Result<()>
{
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
         data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"tail\"}]}}]}\n\n",
    )
    .await?;
    let provider = gemini_provider(GeminiProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..GeminiProviderConfig::default()
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
    let provider = gemini_provider(GeminiProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..GeminiProviderConfig::default()
    })?;
    let mut stream = provider.stream(test_request()).await?;
    let error = stream
        .next()
        .await
        .expect("stream item")
        .expect_err("invalid json should fail");
    assert!(error.to_string().contains("invalid Gemini stream JSON"));
    assert!(stream.next().await.is_none());

    let server = TinySseServer::start_bytes(
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\ndata: \xff\n\n",
    )
    .await?;
    let provider = gemini_provider(GeminiProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..GeminiProviderConfig::default()
    })?;
    let mut stream = provider.stream(test_request()).await?;
    let error = stream
        .next()
        .await
        .expect("stream item")
        .expect_err("invalid utf8 should fail");
    assert!(error.to_string().contains("invalid UTF-8 SSE chunk"));
    assert!(stream.next().await.is_none());
    Ok(())
}

#[tokio::test]
async fn provider_stream_reports_body_read_and_unterminated_frame_errors() -> anyhow::Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 64\r\n\r\n",
    )
    .await?;
    let provider = gemini_provider(GeminiProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..GeminiProviderConfig::default()
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
    let provider = gemini_provider(GeminiProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..GeminiProviderConfig::default()
    })?;
    let mut stream = provider.stream(test_request()).await?;
    let error = stream
        .next()
        .await
        .expect("stream item")
        .expect_err("unterminated invalid frame should fail");
    assert!(error.to_string().contains("invalid Gemini SSE chunk"));
    assert!(stream.next().await.is_none());
    Ok(())
}

#[tokio::test]
async fn provider_stream_retries_retryable_status_once() -> anyhow::Result<()> {
    let server = TinySseServer::start_sequence(vec![
        "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 4\r\n\r\nbusy".as_bytes(),
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
         data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}]}}]}\n\n\
         data: [DONE]\n\n"
            .as_bytes(),
    ])
    .await?;
    let provider = gemini_provider(GeminiProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..GeminiProviderConfig::default()
    })?;

    let chunks = provider
        .stream(test_request())
        .await?
        .collect::<Vec<_>>()
        .await;

    assert_eq!(server.request_count(), 2);
    assert!(
        matches!(chunks[0].as_ref().expect("text"), ProviderChunk::TextDelta(text) if text == "ok")
    );
    assert!(matches!(
        chunks[1].as_ref().expect("done"),
        ProviderChunk::Done
    ));
    Ok(())
}

#[tokio::test]
async fn provider_stream_maps_http_error_status_after_retry() -> anyhow::Result<()> {
    let server = TinySseServer::start_sequence(vec![
        "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 7\r\n\r\nlimited".as_bytes(),
        "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 7\r\n\r\nlimited".as_bytes(),
    ])
    .await?;
    let provider = gemini_provider(GeminiProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..GeminiProviderConfig::default()
    })?;

    let error = match provider.stream(test_request()).await {
        Ok(_) => panic!("429 should fail"),
        Err(error) => error,
    };

    assert_eq!(error.to_string(), "Gemini request was rate limited");
    assert_eq!(server.request_count(), 2);
    Ok(())
}

#[test]
fn provider_private_helpers_cover_retry_edges() {
    assert!(super::is_retryable_status(
        &GeminiProviderError::RateLimited
    ));
    assert!(super::is_retryable_status(
        &GeminiProviderError::RetryableStatus(503)
    ));
    assert!(!super::is_retryable_status(
        &GeminiProviderError::Authentication(401)
    ));

    let mut mapper = crate::mapper::StreamMapper::new();
    let mut pending = VecDeque::new();
    let done = super::enqueue_decoded_frames(
        &mut mapper,
        &mut pending,
        vec![
            crate::stream::GeminiSseFrame::Comment,
            crate::stream::GeminiSseFrame::Blank,
        ],
    )
    .expect("comment and blank frames should be ignored");
    assert!(!done);
    assert!(pending.is_empty());
}

fn gemini_provider(config: GeminiProviderConfig) -> anyhow::Result<GeminiProvider> {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::clear();
    GeminiProvider::new(config)
}

fn test_request() -> CompletionRequest {
    CompletionRequest {
        provider_name: "gemini".to_owned(),
        model_name: "gemini-test".to_owned(),
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
    }
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

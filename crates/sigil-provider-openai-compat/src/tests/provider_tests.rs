use std::{
    collections::VecDeque,
    ffi::OsString,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use futures::StreamExt;
use sigil_kernel::{
    CompletionRequest, ModelMessage, ModelRequestTimeouts, Provider, ProviderChunk,
};

use crate::{
    OPENAI_COMPATIBLE_API_KEY_ENV, OpenAiCompatibleProvider, OpenAiCompatibleProviderConfig,
};

fn new_openai_compatible_provider(
    config: OpenAiCompatibleProviderConfig,
) -> Result<OpenAiCompatibleProvider> {
    OpenAiCompatibleProvider::new(config, ModelRequestTimeouts::default())
}

#[tokio::test]
async fn provider_reports_name_capabilities_and_missing_api_key() -> Result<()> {
    let provider = {
        let _guard = crate::test_env::lock();
        let _scope = EnvScope::set_many(&[
            (OPENAI_COMPATIBLE_API_KEY_ENV, "   "),
            ("OPENAI_API_KEY", "   "),
        ]);
        new_openai_compatible_provider(OpenAiCompatibleProviderConfig {
            api_key: None,
            ..OpenAiCompatibleProviderConfig::default()
        })?
    };

    assert_eq!(provider.name(), "openai_compat");
    assert!(provider.capabilities().supports_tool_stream);

    let error = match provider.stream(test_request()).await {
        Ok(_) => panic!("missing api key should fail before network"),
        Err(error) => error,
    };
    assert_eq!(error.to_string(), "OpenAI-compatible request failed");
    let root = error.root_cause().to_string();
    assert_eq!(root, "missing OpenAI-compatible API key");
    Ok(())
}

#[tokio::test]
async fn provider_stream_surfaces_sse_events_from_http_response() -> Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
         data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
         data: {\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\n\
         data: [DONE]\n\n",
    )
    .await?;
    let provider = new_openai_compatible_provider(OpenAiCompatibleProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..OpenAiCompatibleProviderConfig::default()
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
    Ok(())
}

#[tokio::test]
async fn provider_stream_sends_optional_openai_headers() -> Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
         data: [DONE]\n\n",
    )
    .await?;
    let provider = new_openai_compatible_provider(OpenAiCompatibleProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        organization: Some(" org-1 ".to_owned()),
        project: Some("project-1".to_owned()),
        ..OpenAiCompatibleProviderConfig::default()
    })?;

    let chunks = provider
        .stream(test_request())
        .await?
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(
        chunks[0].as_ref().expect("done"),
        ProviderChunk::Done
    ));
    let request = server.request_text();
    let request_lower = request.to_ascii_lowercase();
    assert!(request_lower.contains("openai-organization: org-1"));
    assert!(request_lower.contains("openai-project: project-1"));
    assert!(request_lower.contains("authorization: bearer test-key"));
    assert!(request.contains("\"model\":\"gpt-test\""));
    Ok(())
}

#[tokio::test]
async fn provider_stream_emits_done_when_http_stream_ends_without_done_frame() -> Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
         data: {\"choices\":[{\"delta\":{\"content\":\"tail\"}}]}\n\n",
    )
    .await?;
    let provider = new_openai_compatible_provider(OpenAiCompatibleProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..OpenAiCompatibleProviderConfig::default()
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
async fn provider_stream_flushes_partial_frame_at_http_eof() -> Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
         data: {\"choices\":[{\"delta\":{\"content\":\"tail\"}}]}",
    )
    .await?;
    let provider = new_openai_compatible_provider(OpenAiCompatibleProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..OpenAiCompatibleProviderConfig::default()
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
async fn provider_stream_reports_invalid_json_frame() -> Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
         data: {not-json}\n\n",
    )
    .await?;
    let provider = new_openai_compatible_provider(OpenAiCompatibleProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..OpenAiCompatibleProviderConfig::default()
    })?;

    let mut stream = provider.stream(test_request()).await?;
    let error = stream
        .next()
        .await
        .expect("stream item")
        .expect_err("invalid json should fail");

    assert!(
        error
            .to_string()
            .contains("invalid OpenAI-compatible stream JSON")
    );
    assert!(stream.next().await.is_none());
    Ok(())
}

#[tokio::test]
async fn provider_stream_reports_invalid_partial_frame_at_http_eof() -> Result<()> {
    let server = TinySseServer::start(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
         event: message",
    )
    .await?;
    let provider = new_openai_compatible_provider(OpenAiCompatibleProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..OpenAiCompatibleProviderConfig::default()
    })?;

    let mut stream = provider.stream(test_request()).await?;
    let error = stream
        .next()
        .await
        .expect("stream item")
        .expect_err("invalid partial frame should fail");

    assert!(error.to_string().contains("invalid SSE chunk"));
    assert!(stream.next().await.is_none());
    Ok(())
}

#[tokio::test]
async fn provider_stream_reports_invalid_utf8_chunk() -> Result<()> {
    let server = TinySseServer::start_bytes(
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\ndata: \xff\n\n",
    )
    .await?;
    let provider = new_openai_compatible_provider(OpenAiCompatibleProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..OpenAiCompatibleProviderConfig::default()
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

#[test]
fn enqueue_decoded_frames_ignores_comment_and_blank_frames() -> Result<()> {
    let mut mapper = crate::mapper::StreamMapper::new();
    let mut pending = VecDeque::new();
    let done = super::enqueue_decoded_frames(
        &mut mapper,
        &mut pending,
        vec![super::OpenAiSseFrame::Comment, super::OpenAiSseFrame::Blank],
    )?;

    assert!(!done);
    assert!(pending.is_empty());
    Ok(())
}

#[test]
fn retryable_status_helper_accepts_only_rate_limits_and_server_errors() {
    assert!(super::is_retryable_status(
        &crate::errors::OpenAiCompatibleProviderError::RateLimited
    ));
    assert!(super::is_retryable_status(
        &crate::errors::OpenAiCompatibleProviderError::RetryableStatus(503)
    ));
    assert!(!super::is_retryable_status(
        &crate::errors::OpenAiCompatibleProviderError::Authentication(401)
    ));
}

#[tokio::test]
async fn provider_stream_maps_http_error_status() -> Result<()> {
    let server = TinySseServer::start_sequence(vec![
        "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 7\r\n\r\nlimited".as_bytes(),
        "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 7\r\n\r\nlimited".as_bytes(),
    ])
    .await?;
    let provider = new_openai_compatible_provider(OpenAiCompatibleProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..OpenAiCompatibleProviderConfig::default()
    })?;

    let error = match provider.stream(test_request()).await {
        Ok(_) => panic!("429 should fail"),
        Err(error) => error,
    };

    assert_eq!(
        error.to_string(),
        "OpenAI-compatible request was rate limited"
    );
    assert_eq!(server.request_count(), 2);
    Ok(())
}

#[tokio::test]
async fn provider_stream_retries_retryable_status_once() -> Result<()> {
    let server = TinySseServer::start_sequence(vec![
        "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 4\r\n\r\nbusy".as_bytes(),
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
          data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n\
          data: [DONE]\n\n"
            .as_bytes(),
    ])
    .await?;
    let provider = new_openai_compatible_provider(OpenAiCompatibleProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..OpenAiCompatibleProviderConfig::default()
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

fn test_request() -> CompletionRequest {
    CompletionRequest {
        provider_name: "openai_compat".to_owned(),
        model_name: "gpt-test".to_owned(),
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

struct EnvScope {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvScope {
    fn set_many(values: &[(&'static str, &'static str)]) -> Self {
        let mut saved = Vec::with_capacity(values.len());
        for (name, value) in values {
            saved.push((*name, std::env::var_os(name)));
            unsafe {
                std::env::set_var(name, value);
            }
        }
        Self { saved }
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        for (name, value) in self.saved.drain(..).rev() {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

impl TinySseServer {
    async fn start(response: &'static str) -> Result<Self> {
        Self::start_bytes(response.as_bytes()).await
    }

    async fn start_bytes(response: &'static [u8]) -> Result<Self> {
        Self::start_sequence(vec![response]).await
    }

    async fn start_sequence(responses: Vec<&'static [u8]>) -> Result<Self> {
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

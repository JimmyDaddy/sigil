use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Result;
use futures::StreamExt;
use sigil_kernel::{
    CompletionRequest, FrozenProviderRequestMaterial, ModelMessage, ModelRequestTimeouts, Provider,
    ProviderChunk,
};

use crate::{
    OPENAI_RESPONSES_API_KEY_ENV, OPENAI_RESPONSES_PORTABLE_TARGET_CONTEXT_WINDOW_TOKENS,
    OPENAI_RESPONSES_PORTABLE_TARGET_MODEL, OPENAI_RESPONSES_PORTABLE_TARGET_OUTPUT_TOKENS,
    OpenAiResponsesProvider, OpenAiResponsesProviderConfig,
};

use super::{
    is_official_openai_base_url, openai_responses_server_count_binding, parse_input_token_count,
};

fn openai_responses_provider(
    config: OpenAiResponsesProviderConfig,
) -> Result<OpenAiResponsesProvider> {
    let _guard = crate::test_env::lock();
    OpenAiResponsesProvider::new(config, ModelRequestTimeouts::default())
}

#[tokio::test]
async fn provider_reports_name_capabilities_and_missing_api_key() -> Result<()> {
    let provider = {
        let _guard = crate::test_env::lock();
        let _scope = EnvScope::set(OPENAI_RESPONSES_API_KEY_ENV, " ");
        OpenAiResponsesProvider::new(
            OpenAiResponsesProviderConfig::default(),
            ModelRequestTimeouts::default(),
        )?
    };

    assert_eq!(provider.name(), "openai_responses");
    assert!(provider.capabilities().supports_tool_stream);

    let error = match provider.stream(test_request()).await {
        Ok(_) => anyhow::bail!("missing key should fail before network"),
        Err(error) => error,
    };
    assert_eq!(error.to_string(), "OpenAI Responses request failed");
    assert_eq!(
        error.root_cause().to_string(),
        "missing OpenAI Responses API key"
    );
    Ok(())
}

#[test]
fn only_the_official_openai_endpoint_is_eligible_for_the_proof_contract() {
    assert!(is_official_openai_base_url("https://api.openai.com/v1"));
    assert!(is_official_openai_base_url("https://api.openai.com/v1/"));
    assert!(!is_official_openai_base_url(
        "https://proxy.example.test/v1"
    ));
}

#[tokio::test]
async fn server_count_target_proof_rejects_unpinned_models_before_network_io() -> Result<()> {
    let provider = openai_responses_provider(OpenAiResponsesProviderConfig {
        api_key: Some("test-key".to_owned()),
        ..OpenAiResponsesProviderConfig::default()
    })?;
    let mut request = test_request();
    request.model_name = "gpt-4.1".to_owned();
    request.max_tokens = Some(OPENAI_RESPONSES_PORTABLE_TARGET_OUTPUT_TOKENS);
    let frozen = FrozenProviderRequestMaterial::freeze("test-session", request)?;

    let error = provider
        .prove_portable_compaction_target(frozen)
        .await
        .expect_err("an alias must not become a server-count proof profile");
    assert!(error.to_string().contains("unavailable for model gpt-4.1"));
    Ok(())
}

#[tokio::test]
async fn input_token_count_posts_once_to_the_dedicated_endpoint() -> Result<()> {
    let server = TinySseServer::start(concat!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n",
        "{\"object\":\"response.input_tokens\",\"input_tokens\":42}"
    ))
    .await?;
    let provider = openai_responses_provider(OpenAiResponsesProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..OpenAiResponsesProviderConfig::default()
    })?;

    assert_eq!(provider.input_token_count(&test_request()).await?, 42);

    let request = server.request_text();
    assert!(request.starts_with("POST /responses/input_tokens HTTP/1.1"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer test-key")
    );
    assert!(request.contains("\"model\":\"gpt-test\""));
    assert!(request.contains("\"input\""));
    assert!(!request.contains("\"stream\""));
    assert!(!request.contains("\"max_output_tokens\""));
    Ok(())
}

#[test]
fn server_count_profile_and_response_parser_are_strict() -> Result<()> {
    let binding = openai_responses_server_count_binding(OPENAI_RESPONSES_PORTABLE_TARGET_MODEL);
    assert_eq!(binding.model_name, OPENAI_RESPONSES_PORTABLE_TARGET_MODEL);
    assert!(binding.hosted_parity_profile.is_some());
    assert_eq!(
        OPENAI_RESPONSES_PORTABLE_TARGET_CONTEXT_WINDOW_TOKENS,
        1_047_576
    );
    assert_eq!(OPENAI_RESPONSES_PORTABLE_TARGET_OUTPUT_TOKENS, 32_768);
    assert_eq!(
        parse_input_token_count(br#"{"object":"response.input_tokens","input_tokens":42}"#)?,
        42
    );
    assert!(parse_input_token_count(br#"{"object":"response","input_tokens":42}"#).is_err());
    assert!(parse_input_token_count(br#"{"object":"response.input_tokens"}"#).is_err());
    Ok(())
}

#[tokio::test]
async fn provider_streams_completed_response_and_keeps_native_output_items() -> Result<()> {
    let server = TinySseServer::start(concat!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n",
        "event: response.output_text.delta\n",
        "data: {\"delta\":\"hi\"}\n\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}],\"provider_extension\":{\"retain\":true}}]}}\n\n"
    ))
    .await?;
    let provider = openai_responses_provider(OpenAiResponsesProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..OpenAiResponsesProviderConfig::default()
    })?;

    let chunks = provider
        .stream(test_request())
        .await?
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(
        chunks[0].as_ref().expect("text"), ProviderChunk::TextDelta(text) if text == "hi"
    ));
    assert!(matches!(
        chunks[1].as_ref().expect("state"), ProviderChunk::ContinuationState(state)
            if state.opaque_blob["response_id"] == "resp_1"
                && state.opaque_blob["output_items"][0]["provider_extension"]["retain"] == true
    ));
    assert!(matches!(
        chunks[2].as_ref().expect("done"),
        ProviderChunk::Done
    ));

    let request = server.request_text();
    assert!(request.starts_with("POST /responses HTTP/1.1"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer test-key")
    );
    assert!(request.contains("\"stream\":true"));
    assert!(request.contains("\"store\":false"));
    Ok(())
}

#[tokio::test]
async fn provider_fails_when_the_transport_ends_without_completed_terminal() -> Result<()> {
    let server = TinySseServer::start(concat!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n",
        "event: response.output_text.delta\n",
        "data: {\"delta\":\"partial\"}\n\n"
    ))
    .await?;
    let provider = openai_responses_provider(OpenAiResponsesProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..OpenAiResponsesProviderConfig::default()
    })?;

    let chunks = provider
        .stream(test_request())
        .await?
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(
        chunks[0].as_ref().expect("partial text"), ProviderChunk::TextDelta(text) if text == "partial"
    ));
    let error = chunks[1]
        .as_ref()
        .expect_err("missing terminal must be surfaced");
    assert!(
        error
            .to_string()
            .contains("ended before response.completed")
    );
    Ok(())
}

#[tokio::test]
async fn compact_posts_the_complete_window_once_and_preserves_opaque_output() -> Result<()> {
    let server = TinySseServer::start(concat!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n",
        "{\"id\":\"resp_cmp_1\",\"object\":\"response.compaction\",\"output\":[",
        "{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"user\",\"content\":[]},",
        "{\"id\":\"cmp_1\",\"type\":\"compaction\",\"encrypted_content\":\"opaque\",\"extension\":{\"keep\":true}}]}"
    ))
    .await?;
    let provider = openai_responses_provider(OpenAiResponsesProviderConfig {
        base_url: server.base_url(),
        api_key: Some("test-key".to_owned()),
        ..OpenAiResponsesProviderConfig::default()
    })?;

    let compacted = provider.compact(&test_request()).await?;

    assert_eq!(compacted.response_id, "resp_cmp_1");
    assert_eq!(
        compacted.canonical_output_json(),
        r#"[{"id":"msg_1","type":"message","role":"user","content":[]},{"id":"cmp_1","type":"compaction","encrypted_content":"opaque","extension":{"keep":true}}]"#
    );
    let request = server.request_text();
    assert!(request.starts_with("POST /responses/compact HTTP/1.1"));
    assert!(request.contains("\"model\":\"gpt-test\""));
    assert!(request.contains("\"input\""));
    assert!(!request.contains("\"stream\""));
    Ok(())
}

fn test_request() -> CompletionRequest {
    CompletionRequest {
        provider_name: "openai_responses".to_owned(),
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
        hosted_tools: Vec::new(),
    }
}

struct TinySseServer {
    address: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl TinySseServer {
    async fn start(response: &'static str) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let task_requests = Arc::clone(&requests);
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            loop {
                let Ok(Ok(read)) = tokio::time::timeout(
                    Duration::from_millis(500),
                    tokio::io::AsyncReadExt::read(&mut socket, &mut buffer),
                )
                .await
                else {
                    break;
                };
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if let Some(header_end) = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|index| index + 4)
                {
                    let content_length = content_length(&request[..header_end]).unwrap_or_default();
                    if request.len() >= header_end + content_length {
                        break;
                    }
                }
            }
            task_requests
                .lock()
                .expect("request capture mutex should not be poisoned")
                .push(request);
            let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
        });
        Ok(Self { address, requests })
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn request_text(&self) -> String {
        String::from_utf8(
            self.requests
                .lock()
                .expect("request capture mutex should not be poisoned")[0]
                .clone(),
        )
        .expect("request should be UTF-8")
    }
}

fn content_length(headers: &[u8]) -> Option<usize> {
    std::str::from_utf8(headers).ok()?.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

struct EnvScope {
    name: &'static str,
    saved: Option<std::ffi::OsString>,
}

impl EnvScope {
    fn set(name: &'static str, value: &'static str) -> Self {
        let saved = std::env::var_os(name);
        unsafe {
            std::env::set_var(name, value);
        }
        Self { name, saved }
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        unsafe {
            match self.saved.take() {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }
}

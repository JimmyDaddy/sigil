use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use futures::StreamExt;
use termquill_kernel::{Provider, ProviderChunk, ToolCall, ToolSpec};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::{Duration, sleep, timeout},
};

use crate::{
    DeepSeekFimCompletionRequest, DeepSeekPrefixCompletionRequest, request::build_chat_request,
    stream::test_support::parse_sse_frames,
};

use super::DeepSeekProvider;

#[test]
fn request_body_injects_reasoning_replay_into_matching_assistant_message() -> Result<()> {
    let assistant = termquill_kernel::ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{}".to_owned(),
        }],
    );
    let assistant_id = assistant.id.clone();
    let request = termquill_kernel::CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        messages: vec![assistant],
        tools: vec![ToolSpec {
            name: "read_file".to_owned(),
            description: "read".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            read_only: true,
        }],
        temperature: None,
        max_tokens: None,
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: vec![termquill_kernel::ProviderContinuationState {
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
        request_timeout_secs: 10,
    };
    let provider = DeepSeekProvider::new(config.clone())?;
    let request = termquill_kernel::CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: config.model.clone(),
        messages: vec![termquill_kernel::ModelMessage::user("hi")],
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: vec![termquill_kernel::ProviderContinuationState {
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
        request_timeout_secs: 10,
    };
    let provider = DeepSeekProvider::new(config.clone())?;
    let request = termquill_kernel::CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: config.model.clone(),
        messages: vec![termquill_kernel::ModelMessage::user("hi")],
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
        request_timeout_secs: 10,
    };
    let provider = DeepSeekProvider::new(config.clone())?;
    let request = termquill_kernel::CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: config.model.clone(),
        messages: vec![termquill_kernel::ModelMessage::user("hi")],
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
async fn prefix_completion_uses_beta_chat_path() -> Result<()> {
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"delta\":{\"content\":\"prefixed\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )])));
    let server = spawn_recording_server(Arc::clone(&requests), Arc::clone(&responses)).await?;
    let provider = DeepSeekProvider::new(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server.clone(),
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
        request_timeout_secs: 10,
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
        "data: {\"choices\":[{\"text\":\"middle\",\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )])));
    let server = spawn_recording_server(Arc::clone(&requests), Arc::clone(&responses)).await?;
    let provider = DeepSeekProvider::new(crate::DeepSeekProviderConfig {
        base_url: server.clone(),
        beta_base_url: server.clone(),
        anthropic_base_url: server.clone(),
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: crate::StrictToolsMode::Auto,
        request_timeout_secs: 10,
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
    let raw_request = requests
        .lock()
        .expect("requests poisoned")
        .pop_front()
        .expect("expected recorded fim request");
    assert!(raw_request.contains("POST /completions"));
    assert!(raw_request.contains("\"suffix\":\"\\n}\\n\""));
    Ok(())
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

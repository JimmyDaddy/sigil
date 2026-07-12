use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};

use anyhow::Result;
use futures::StreamExt;
use serde_json::json;
use sigil_kernel::{
    AgentConfig, CodeIntelligenceConfig, ExecutionConfig, McpServerConfig, MemoryConfig,
    PermissionConfig, ProviderChunk, RootConfig, SessionConfig, TaskConfig, VerificationConfig,
    WorkspaceConfig,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::{
    DeepSeekFimDebugRequest, DeepSeekPrefixDebugRequest, stream_deepseek_fim_debug,
    stream_deepseek_prefix_debug,
};

#[tokio::test]
async fn prefix_debug_stream_routes_through_runtime_adapter() -> Result<()> {
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"delta\":{\"content\":\"prefixed\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )])));
    let server = spawn_recording_server(Arc::clone(&requests), Arc::clone(&responses)).await?;
    let workspace = tempfile::tempdir()?;
    let config_path = workspace.path().join("sigil.toml");
    let root_config = test_root_config(&server);

    let mut stream = stream_deepseek_prefix_debug(
        &root_config,
        &config_path,
        workspace.path(),
        DeepSeekPrefixDebugRequest {
            prompt: "write code".to_owned(),
            assistant_prefix: "```rust\n".to_owned(),
            stop: vec!["```".to_owned()],
            model: Some("deepseek-v4-flash".to_owned()),
        },
    )
    .await?;

    drain_stream(&mut stream).await?;

    let raw_request = requests
        .lock()
        .expect("requests poisoned")
        .pop_front()
        .expect("expected recorded prefix request");
    assert!(raw_request.contains("POST /chat/completions"));
    assert!(raw_request.contains("\"prefix\":true"));
    assert!(raw_request.contains("```rust"));
    assert!(raw_request.contains("\"user_id\":\"workspace-"));
    assert!(!raw_request.contains("local-user"));
    Ok(())
}

#[tokio::test]
async fn fim_debug_stream_routes_through_runtime_adapter() -> Result<()> {
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"text\":\"middle\",\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3,\"prompt_cache_hit_tokens\":2,\"prompt_cache_miss_tokens\":5},\"system_fingerprint\":\"fp-fim\"}\n\ndata: [DONE]\n\n",
    )])));
    let server = spawn_recording_server(Arc::clone(&requests), Arc::clone(&responses)).await?;
    let root_config = test_root_config(&server);

    let mut stream = stream_deepseek_fim_debug(
        &root_config,
        DeepSeekFimDebugRequest {
            prompt: "fn main() {\n".to_owned(),
            suffix: "\n}\n".to_owned(),
            stop: vec!["STOP".to_owned()],
            model: Some("deepseek-v4-pro".to_owned()),
            max_tokens: Some(32),
        },
    )
    .await?;

    drain_stream(&mut stream).await?;

    let raw_request = requests
        .lock()
        .expect("requests poisoned")
        .pop_front()
        .expect("expected recorded fim request");
    assert!(raw_request.contains("POST /completions"));
    assert!(raw_request.contains("\"suffix\":\"\\n}\\n\""));
    assert!(raw_request.contains("\"max_tokens\":32"));
    Ok(())
}

fn test_root_config(base_url: &str) -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig::default(),
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 5,
        },
        permission: PermissionConfig::default(),
        model_request: Default::default(),
        memory: MemoryConfig::default(),
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: CodeIntelligenceConfig::default(),
        terminal: Default::default(),
        execution: ExecutionConfig::default(),
        verification: VerificationConfig::default(),
        appearance: Default::default(),
        task: TaskConfig::default(),
        providers: BTreeMap::from([(
            "deepseek".to_owned(),
            json!({
                "base_url": base_url,
                "beta_base_url": base_url,
                "anthropic_base_url": base_url,
                "fim_model": "deepseek-v4-pro",
                "api_key": "test-key",
                "strict_tools_mode": "auto"
            }),
        )]),
        web: Default::default(),
        mcp_servers: Vec::<McpServerConfig>::new(),
    }
}

async fn drain_stream(stream: &mut super::ProviderDebugStream) -> Result<Vec<ProviderChunk>> {
    let mut chunks = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let stop = matches!(chunk, ProviderChunk::Done);
        chunks.push(chunk);
        if stop {
            break;
        }
    }
    Ok(chunks)
}

fn http_response(status: u16, content_type: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
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
                let mut buffer = vec![0; 8192];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                requests
                    .lock()
                    .expect("requests poisoned")
                    .push_back(request);
                let response = responses
                    .lock()
                    .expect("responses poisoned")
                    .pop_front()
                    .unwrap_or_else(|| http_response(500, "text/plain", "unexpected request"));
                let _ = socket.write_all(&response).await;
            });
        }
    });
    Ok(format!("http://{address}"))
}

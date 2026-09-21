use std::time::Duration;

use anyhow::{Context, Result, ensure};
use futures::StreamExt;
use serde_json::Value;
use sigil_kernel::{
    CompletionRequest, ModelMessage, ModelRequestTimeouts, Provider, REQUEST_USER_INPUT_TOOL_NAME,
    ToolRegistry, ToolSpec, UPDATE_TASK_CHECKLIST_TOOL_NAME, request_user_input_tool_spec,
    update_task_checklist_tool_spec,
};
use sigil_provider_deepseek::{DeepSeekProvider, DeepSeekProviderConfig, StrictToolsMode};
use sigil_tools_builtin::{
    BuiltinToolPaths, register_builtin_tools_with_unavailable_managed_execution,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn builtin_and_checklist_schemas_reach_provider_without_strict_fallback() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let mut registry = ToolRegistry::new();
    let _owners = register_builtin_tools_with_unavailable_managed_execution(
        &mut registry,
        BuiltinToolPaths::workspace_defaults(workspace.path()),
    );
    let mut specs = registry.specs();
    specs.push(update_task_checklist_tool_spec());
    specs.push(request_user_input_tool_spec());
    for name in [
        "read_tool_artifact",
        "exec_command",
        "apply_changeset",
        UPDATE_TASK_CHECKLIST_TOOL_NAME,
        REQUEST_USER_INPUT_TOOL_NAME,
    ] {
        ensure!(
            specs.iter().any(|spec| spec.name == name),
            "missing conformance tool {name}"
        );
    }
    for mode in [
        StrictToolsMode::Auto,
        StrictToolsMode::Always,
        StrictToolsMode::Off,
    ] {
        let body = tokio::time::timeout(Duration::from_secs(10), capture_request(&specs, mode))
            .await
            .context("tool schema conformance fixture timed out")??;
        let tools = body["tools"].as_array().context("provider tools missing")?;
        assert_eq!(tools.len(), specs.len());
        for tool in tools {
            let function = &tool["function"];
            if mode == StrictToolsMode::Off {
                assert!(function.get("strict").is_none());
                let original = specs
                    .iter()
                    .find(|spec| function["name"].as_str() == Some(spec.name.as_str()))
                    .context("unexpected provider tool")?;
                assert_eq!(function["parameters"], original.input_schema);
            } else {
                assert_eq!(
                    function["strict"], true,
                    "strict fallback for {}",
                    function["name"]
                );
            }
        }
    }
    Ok(())
}

async fn capture_request(specs: &[ToolSpec], mode: StrictToolsMode) -> Result<Value> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    let provider = DeepSeekProvider::new_exact_with_client(
        DeepSeekProviderConfig {
            base_url: base_url.clone(),
            beta_base_url: base_url.clone(),
            anthropic_base_url: base_url,
            api_key: Some("schema-fixture-key".to_owned()),
            strict_tools_mode: mode,
            ..DeepSeekProviderConfig::default()
        },
        ModelRequestTimeouts::default(),
        reqwest::Client::builder().no_proxy().build()?,
    )?;
    let request = CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        messages: vec![ModelMessage::user("schema conformance fixture")],
        tools: specs.to_vec(),
        temperature: None,
        max_tokens: Some(32),
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    };
    let server = async {
        let (mut socket, _) = listener.accept().await?;
        let mut bytes = Vec::new();
        let (header_end, content_length) = loop {
            let mut chunk = [0; 4096];
            let count = socket.read(&mut chunk).await?;
            ensure!(count > 0, "fixture request ended before headers");
            bytes.extend_from_slice(&chunk[..count]);
            ensure!(
                bytes.len() <= 2 * 1024 * 1024,
                "fixture request exceeds bound"
            );
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&bytes[..index])?;
                let content_length = headers
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .context("fixture content length missing")?
                    .1
                    .trim()
                    .parse::<usize>()?;
                ensure!(
                    content_length <= 2 * 1024 * 1024,
                    "fixture body exceeds bound"
                );
                break (index + 4, content_length);
            }
        };
        while bytes.len() < header_end + content_length {
            let mut chunk = [0; 4096];
            let count = socket.read(&mut chunk).await?;
            ensure!(count > 0, "fixture request ended before body");
            bytes.extend_from_slice(&chunk[..count]);
        }
        let body = serde_json::from_slice(&bytes[header_end..header_end + content_length])?;
        let response = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await?;
        socket.shutdown().await?;
        Ok::<_, anyhow::Error>(body)
    };
    let client = async {
        let mut stream = provider.stream(request).await?;
        while let Some(chunk) = stream.next().await {
            chunk?;
        }
        Ok::<_, anyhow::Error>(())
    };
    let (body, ()) = tokio::try_join!(server, client)?;
    Ok(body)
}

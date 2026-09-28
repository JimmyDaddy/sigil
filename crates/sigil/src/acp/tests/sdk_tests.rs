use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "approval_tests.rs"]
mod approval_tests;

#[tokio::test]
async fn sdk_cancelled_provider_wait_returns_cancelled_and_durable_cleanup_receipt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().canonicalize()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let config_path = workspace.join("sigil.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2
[workspace]
root = "."
[storage]
state_root = "{}"
cache_root = "{}"
[agent]
connection = "local-test"
model = "gpt-4.1"
[model_request]
request_timeout_secs = 30
[task]
routing_policy = "manual"
[connections.local-test]
label = "ACP test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://{address}"
credential = {{ source = "none" }}
"#,
            workspace.join("state").display(),
            workspace.join("cache").display()
        ),
    )?;
    sigil_kernel::RootConfig::load(&config_path).context("valid protocol fixture config")?;
    let (started, provider_started) = tokio::sync::oneshot::channel();
    let (stop_provider, mut stopped) = tokio::sync::oneshot::channel();
    let provider = tokio::spawn(async move {
        let (mut socket, _) = tokio::select! {
            accepted = listener.accept() => accepted?,
            _ = &mut stopped => return Ok::<_, anyhow::Error>(()),
        };
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        let body_start = loop {
            let count = socket.read(&mut buffer).await?;
            ensure!(count > 0, "provider request ended before headers");
            request.extend_from_slice(&buffer[..count]);
            if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..body_start]);
        let content_length: usize = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then_some(value.trim())
            })
            .context("request content length")?
            .parse()?;
        while request.len() - body_start < content_length {
            let count = socket.read(&mut buffer).await?;
            ensure!(count > 0, "provider request ended before body");
            request.extend_from_slice(&buffer[..count]);
        }
        let body: serde_json::Value = serde_json::from_slice(&request[body_start..])?;
        ensure!(
            body.to_string().contains("acp-resource.txt"),
            "resource reference reaches the real provider request"
        );
        let event = "data: {\"id\":\"fixture\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4.1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n";
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await?;
        socket
            .write_all(format!("{:x}\r\n{event}\r\n", event.len()).as_bytes())
            .await?;
        let _ = started.send(());
        let _ = stopped.await;
        Ok::<_, anyhow::Error>(())
    });
    let (agent_transport, client_transport) = agent_client_protocol::Channel::duplex();
    let server_config = config_path.clone();
    let server =
        tokio::spawn(async move { serve_transport(&server_config, agent_transport).await });
    let client_result = tokio::time::timeout(Duration::from_secs(30),
        Client.builder()
            .on_receive_notification(
                async move |_notification: acp::SessionNotification, _connection| Ok(()),
                agent_client_protocol::on_receive_notification!(),
            )
            .connect_with(client_transport, async move |client| {
                let initialized = client.send_request(acp::InitializeRequest::new(
                    agent_client_protocol::schema::ProtocolVersion::V1,
                )).block_task().await?;
                assert_eq!(initialized.protocol_version, agent_client_protocol::schema::ProtocolVersion::V1);
                let first = client.send_request(acp::NewSessionRequest::new(&workspace)).block_task().await?;
                let second = client.send_request(acp::NewSessionRequest::new(&workspace)).block_task().await?;
                assert_ne!(first.session_id, second.session_id);
                let response = client.send_request(acp::PromptRequest::new(first.session_id.clone(), vec![
                    acp::ContentBlock::Text(acp::TextContent::new("Read the supplied reference when useful.")),
                    acp::ContentBlock::ResourceLink(acp::ResourceLink::new("source", "file:///unavailable/acp-resource.txt")),
                ])).block_task();
                tokio::pin!(response);
                tokio::select! {
                    started = provider_started => started.map_err(protocol_error)?,
                    response = &mut response => panic!("turn ended before provider cancellation: {response:?}"),
                }
                client.send_notification(acp::CancelNotification::new(first.session_id))?;
                let response = response.await?;
                assert_eq!(response.stop_reason, acp::StopReason::Cancelled);
                Ok(())
            })
    ).await.context("SDK cancellation deadline");
    let _ = stop_provider.send(());
    // Always join both actual owners, including when a protocol assertion returns an error.
    provider.await.context("fixture provider owner")??;
    server.await.context("ACP adapter owner")??;
    client_result??;
    let mut receipts = Vec::new();
    for entry in walkdir::WalkDir::new(temp.path().join("state")) {
        let entry = entry?;
        if entry.file_name() != "records.jsonl" {
            continue;
        }
        for line in std::fs::read_to_string(entry.path())?.lines() {
            let record: serde_json::Value = serde_json::from_str(line)?;
            if record["event_type"] == "run_finalized" && record["payload"]["record"] == "finalized"
            {
                receipts.push(record["payload"].clone());
            }
        }
    }
    assert_eq!(
        receipts.len(),
        1,
        "exactly one actual cancellation finalization"
    );
    assert_eq!(receipts[0]["outcome"], "cancelled");
    assert_eq!(receipts[0]["cleanup_complete"], true);
    assert_eq!(receipts[0]["active_tasks"], 0);
    assert_eq!(receipts[0]["active_effects"], 0);
    Ok(())
}

fn write_sdk_config(workspace: &Path, address: std::net::SocketAddr) -> Result<PathBuf> {
    let config = workspace.join("sigil.toml");
    std::fs::write(
        &config,
        format!(
            r#"config_version = 2
[workspace]
root = "."
[storage]
state_root = "{}"
cache_root = "{}"
[agent]
connection = "local-test"
model = "gpt-4.1"
[model_request]
request_timeout_secs = 20
[task]
routing_policy = "manual"
[connections.local-test]
label = "ACP restart test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://{address}"
credential = {{ source = "none" }}
"#,
            workspace.join("state").display(),
            workspace.join("cache").display()
        ),
    )?;
    sigil_kernel::RootConfig::load(&config)?;
    Ok(config)
}

async fn read_provider_request(socket: &mut tokio::net::TcpStream) -> Result<serde_json::Value> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    let body_start = loop {
        let count = socket.read(&mut buffer).await?;
        ensure!(count > 0, "provider request ended before headers");
        request.extend_from_slice(&buffer[..count]);
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8_lossy(&request[..body_start]);
    let content_length: usize = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then_some(value.trim())
        })
        .context("request content length")?
        .parse()?;
    while request.len() - body_start < content_length {
        let count = socket.read(&mut buffer).await?;
        ensure!(count > 0, "provider request ended before body");
        request.extend_from_slice(&buffer[..count]);
    }
    Ok(serde_json::from_slice(
        &request[body_start..body_start + content_length],
    )?)
}

#[tokio::test]
async fn sdk_restart_loads_exact_workspace_session_and_continues_real_history() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().canonicalize()?;
    let foreign_temp = tempfile::tempdir()?;
    let foreign = foreign_temp.path().canonicalize()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let config = write_sdk_config(&workspace, listener.local_addr()?)?;
    let (stop, mut stopped) = watch::channel(false);
    let provider = tokio::spawn(async move {
        let mut requests = Vec::new();
        loop {
            let (mut socket, _) = tokio::select! {
                accepted = listener.accept() => accepted?,
                _ = stopped.changed() => break,
            };
            let request = tokio::select! {
                request = read_provider_request(&mut socket) => request?,
                _ = stopped.changed() => break,
            };
            requests.push(request);
            let event = "data: {\"id\":\"fixture\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4.1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"durable reply amber-482\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"fixture\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4.1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":20,\"completion_tokens\":5,\"total_tokens\":25}}\n\ndata: [DONE]\n\n";
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{event}", event.len()).as_bytes()).await?;
        }
        Ok::<_, anyhow::Error>(requests)
    });
    let result = async {
        let (agent_transport, client_transport) = agent_client_protocol::Channel::duplex();
        let server_config = config.clone();
        let server =
            tokio::spawn(async move { serve_transport(&server_config, agent_transport).await });
        let first_workspace = workspace.clone();
        let first = tokio::time::timeout(
            Duration::from_secs(30),
            Client
                .builder()
                .on_receive_notification(
                    async move |_notification: acp::SessionNotification, _connection| Ok(()),
                    agent_client_protocol::on_receive_notification!(),
                )
                .connect_with(client_transport, async move |client| {
                    let initialized = client
                        .send_request(acp::InitializeRequest::new(
                            agent_client_protocol::schema::ProtocolVersion::V1,
                        ))
                        .block_task()
                        .await?;
                    if !initialized.agent_capabilities.load_session {
                        return Err(protocol_error("loadSession not advertised"));
                    }
                    let session = client
                        .send_request(acp::NewSessionRequest::new(first_workspace))
                        .block_task()
                        .await?;
                    let response = client
                        .send_request(acp::PromptRequest::new(
                            session.session_id.clone(),
                            vec![acp::ContentBlock::Text(acp::TextContent::new(
                                "Remember cobalt-719 in this conversation.",
                            ))],
                        ))
                        .block_task()
                        .await?;
                    if response.stop_reason != acp::StopReason::EndTurn {
                        return Err(protocol_error("first turn did not complete"));
                    }
                    Ok(session.session_id)
                }),
        )
        .await;
        server.await.context("first ACP adapter joined")??;
        let id = first.context("first SDK deadline")??;

        // A fresh Adapter (fresh service/attachment owners) has no in-memory session mapping.
        let (agent_transport, client_transport) = agent_client_protocol::Channel::duplex();
        let server_config = config.clone();
        let server =
            tokio::spawn(async move { serve_transport(&server_config, agent_transport).await });
        let received = Arc::new(Mutex::new(Vec::<acp::SessionNotification>::new()));
        let notifications = Arc::clone(&received);
        let check_notifications = Arc::clone(&received);
        let resumed = tokio::time::timeout(
            Duration::from_secs(30),
            Client
                .builder()
                .on_receive_notification(
                    async move |notification: acp::SessionNotification, _connection| {
                        notifications
                            .lock()
                            .map_err(|_| acp::Error::internal_error())?
                            .push(notification);
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .connect_with(client_transport, async move |client| {
                    client
                        .send_request(acp::InitializeRequest::new(
                            agent_client_protocol::schema::ProtocolVersion::V1,
                        ))
                        .block_task()
                        .await?;
                    let text = id.0.as_ref();
                    let (prefix_nonce, scope) = text
                        .rsplit_once(':')
                        .ok_or_else(|| protocol_error("fixture identity"))?;
                    let bad_nonce =
                        format!("sigil-acp-v1:{}:{scope}", uuid::Uuid::new_v4().simple());
                    let bad_scope = format!("{prefix_nonce}:different-durable-scope");
                    for request in [
                        acp::LoadSessionRequest::new(id.clone(), foreign),
                        acp::LoadSessionRequest::new(bad_nonce, &workspace),
                        acp::LoadSessionRequest::new(bad_scope, &workspace),
                    ] {
                        if client.send_request(request).block_task().await.is_ok() {
                            return Err(protocol_error("foreign/tampered session was loaded"));
                        }
                    }
                    if !check_notifications
                        .lock()
                        .map_err(|_| acp::Error::internal_error())?
                        .is_empty()
                    {
                        return Err(protocol_error("history leaked before exact validation"));
                    }
                    client
                        .send_request(acp::LoadSessionRequest::new(id.clone(), &workspace))
                        .block_task()
                        .await?;
                    // Reload in the same connection reuses the actual attachment, not a second writer.
                    client
                        .send_request(acp::LoadSessionRequest::new(id.clone(), &workspace))
                        .block_task()
                        .await?;
                    let response = client
                        .send_request(acp::PromptRequest::new(
                            id.clone(),
                            vec![acp::ContentBlock::Text(acp::TextContent::new(
                                "Continue the same conversation: what did I ask you to remember?",
                            ))],
                        ))
                        .block_task()
                        .await?;
                    if response.stop_reason != acp::StopReason::EndTurn {
                        return Err(protocol_error("resumed turn did not complete"));
                    }
                    Ok(id)
                }),
        )
        .await;
        server.await.context("reopened ACP adapter joined")??;
        let resumed_id = resumed.context("resume SDK deadline")??;
        let notifications = received
            .lock()
            .map_err(|_| anyhow!("fixture notifications"))?;
        ensure!(
            notifications
                .iter()
                .all(|notification| notification.session_id == resumed_id),
            "public ACP identity was replaced by durable scope"
        );
        let history = serde_json::to_string(&*notifications)?;
        ensure!(
            history.contains("cobalt-719") && history.contains("durable reply amber-482"),
            "durable user and assistant history missing from loadSession"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    stop.send_replace(true);
    let requests = provider.await.context("restart provider joined")??;
    result?;
    let (titles, prompts): (Vec<_>, Vec<_>) = requests
        .iter()
        .partition(|request| is_title_request(request));
    assert_eq!(
        titles.len(),
        1,
        "one explicit first-turn semantic-title maintenance"
    );
    assert_eq!(
        prompts.len(),
        2,
        "loadSession must not add a foreground model round"
    );
    assert!(prompts[1]["messages"].to_string().contains("cobalt-719"));
    assert!(
        prompts[1]["messages"]
            .to_string()
            .contains("durable reply amber-482")
    );
    Ok(())
}

fn is_title_request(request: &serde_json::Value) -> bool {
    request["max_tokens"] == 64
        && request
            .get("tools")
            .is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty))
        && request["messages"]
            .as_array()
            .is_some_and(|messages| messages.len() == 2 && messages[0]["role"] == "system")
}

async fn reply_from_fixture_provider(socket: &mut tokio::net::TcpStream) -> Result<()> {
    let event = "data: {\"id\":\"fixture\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4.1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"completed\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"fixture\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4.1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":20,\"completion_tokens\":5,\"total_tokens\":25}}\n\ndata: [DONE]\n\n";
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{event}", event.len()).as_bytes()).await?;
    Ok(())
}

#[tokio::test]
async fn sdk_title_wait_does_not_block_terminal_or_next_prompt_and_disconnect_joins_it()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().canonicalize()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let config = write_sdk_config(&workspace, listener.local_addr()?)?;
    let release_title = Arc::new(tokio::sync::Notify::new());
    let provider_release = Arc::clone(&release_title);
    let (title_entered, title_started) = tokio::sync::oneshot::channel();
    let (stop, mut stopped) = watch::channel(false);
    let provider = tokio::spawn(async move {
        let mut title_entered = Some(title_entered);
        let mut pending_titles = tokio::task::JoinSet::new();
        let mut requests = Vec::new();
        loop {
            let (mut socket, _) = tokio::select! {
                accepted = listener.accept() => accepted?,
                _ = stopped.changed() => break,
            };
            let request = tokio::select! {
                request = read_provider_request(&mut socket) => request?,
                _ = stopped.changed() => break,
            };
            let title = is_title_request(&request);
            requests.push(request);
            if title {
                let release = Arc::clone(&provider_release);
                if let Some(entered) = title_entered.take() {
                    let _ = entered.send(());
                }
                pending_titles.spawn(async move {
                    release.notified().await;
                    reply_from_fixture_provider(&mut socket).await
                });
            } else {
                reply_from_fixture_provider(&mut socket).await?;
            }
        }
        while let Some(result) = pending_titles.join_next().await {
            result??;
        }
        Ok::<_, anyhow::Error>(requests)
    });
    let (agent_transport, client_transport) = agent_client_protocol::Channel::duplex();
    let mut server = tokio::spawn(async move { serve_transport(&config, agent_transport).await });
    let client_result = tokio::time::timeout(
        Duration::from_secs(10),
        Client
            .builder()
            .on_receive_notification(
                async move |_notification: acp::SessionNotification, _connection| Ok(()),
                agent_client_protocol::on_receive_notification!(),
            )
            .connect_with(client_transport, async move |client| {
                client
                    .send_request(acp::InitializeRequest::new(
                        agent_client_protocol::schema::ProtocolVersion::V1,
                    ))
                    .block_task()
                    .await?;
                let session = client
                    .send_request(acp::NewSessionRequest::new(workspace))
                    .block_task()
                    .await?;
                let first = tokio::time::timeout(
                    Duration::from_secs(5),
                    client
                        .send_request(acp::PromptRequest::new(
                            session.session_id.clone(),
                            vec![acp::ContentBlock::Text(acp::TextContent::new(
                                "First ordinary prompt.",
                            ))],
                        ))
                        .block_task(),
                )
                .await
                .map_err(protocol_error)??;
                if first.stop_reason != acp::StopReason::EndTurn {
                    return Err(protocol_error("first foreground not terminal"));
                }
                title_started.await.map_err(protocol_error)?;
                let second = client
                    .send_request(acp::PromptRequest::new(
                        session.session_id,
                        vec![acp::ContentBlock::Text(acp::TextContent::new(
                            "Next ordinary prompt while title remains blocked.",
                        ))],
                    ))
                    .block_task()
                    .await?;
                if second.stop_reason != acp::StopReason::EndTurn {
                    return Err(protocol_error("second foreground was blocked by title"));
                }
                Ok(())
            }),
    )
    .await;
    // The peer has closed, but the original retained owner must still join its gated request.
    let early_shutdown = tokio::time::timeout(Duration::from_millis(100), &mut server).await;
    let retained_until_release = early_shutdown.is_err();
    release_title.notify_one();
    let server_result = match early_shutdown {
        Ok(result) => result,
        Err(_) => server.await,
    };
    stop.send_replace(true);
    let requests = provider.await.context("title fixture provider joined")??;
    server_result.context("title fixture ACP owner joined")??;
    client_result.context("foreground unexpectedly waited for non-critical title")??;
    ensure!(
        retained_until_release,
        "disconnect detached the pending title owner"
    );
    let (titles, prompts): (Vec<_>, Vec<_>) = requests
        .iter()
        .partition(|request| is_title_request(request));
    assert_eq!(titles.len(), 1);
    assert_eq!(prompts.len(), 2);
    Ok(())
}

#[tokio::test]
async fn sdk_retained_completed_run_neither_blocks_next_prompt_nor_cancels_it() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().canonicalize()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let config = write_sdk_config(&workspace, listener.local_addr()?)?;
    let adapter = Arc::new(Adapter::new(config.clone()));
    adapter.initialized.store(true, Ordering::Release);
    let session_id = adapter
        .new_session(acp::NewSessionRequest::new(workspace))
        .await?
        .session_id;
    let session = adapter.session(&session_id).map_err(protocol_error)?;
    let release_second = Arc::new(tokio::sync::Notify::new());
    let provider_release = Arc::clone(&release_second);
    let (second_entered, second_started) = tokio::sync::oneshot::channel();
    let (stop, mut stopped) = watch::channel(false);
    let provider = tokio::spawn(async move {
        let mut second_entered = Some(second_entered);
        let mut foreground_requests = 0;
        loop {
            let (mut socket, _) = tokio::select! {
                accepted = listener.accept() => accepted?,
                _ = stopped.changed() => break,
            };
            let request = read_provider_request(&mut socket).await?;
            if !is_title_request(&request) {
                foreground_requests += 1;
                if foreground_requests == 2 {
                    if let Some(entered) = second_entered.take() {
                        let _ = entered.send(());
                    }
                    tokio::select! {
                        _ = provider_release.notified() => {},
                        _ = stopped.changed() => break,
                    }
                }
            }
            reply_from_fixture_provider(&mut socket).await?;
        }
        Ok::<_, anyhow::Error>(foreground_requests)
    });
    let client = Client.builder().on_receive_notification(
        async move |_notification: acp::SessionNotification, _connection| Ok(()),
        agent_client_protocol::on_receive_notification!(),
    );
    let prompt_release = Arc::clone(&release_second);
    let result = Agent
        .builder()
        .connect_with(client, async move |client| {
            async {
                let first_run = Arc::new(Run::new());
                // A cancel callback can clone the active Run before the original prompt clears
                // it, then reach its owned blocking worker only after that prompt completes.
                let late_cancel = Arc::clone(&first_run);
                let (first, first_maintenance) = execute_prompt(
                    config.clone(),
                    Arc::clone(&session),
                    first_run,
                    "First ordinary prompt.".to_owned(),
                    client.clone(),
                )
                .await?;
                ensure!(first.stop_reason == acp::StopReason::EndTurn);
                let second = execute_prompt(
                    config,
                    session,
                    Arc::new(Run::new()),
                    "Next ordinary prompt while the old cancel callback is retained.".to_owned(),
                    client,
                );
                tokio::pin!(second);
                tokio::select! {
                    started = second_started => started.context("second request started")?,
                    result = &mut second => {
                        result?;
                        anyhow::bail!("second prompt ended before its provider gate");
                    },
                }
                tokio::task::spawn_blocking(move || late_cancel.cancel()).await?;
                prompt_release.notify_one();
                let (second, second_maintenance) = second.await?;
                ensure!(
                    second.stop_reason == acp::StopReason::EndTurn,
                    "late cancellation affected the next foreground run"
                );
                for maintenance in [first_maintenance, second_maintenance]
                    .into_iter()
                    .flatten()
                {
                    maintenance.execute().await?;
                }
                Ok::<_, anyhow::Error>(())
            }
            .await
            .map_err(protocol_error)
        })
        .await;
    release_second.notify_one();
    stop.send_replace(true);
    let requests = provider.await.context("provider joined")??;
    let cleanup = adapter.shutdown().await;
    result?;
    cleanup?;
    assert_eq!(requests, 2, "both actual foreground requests must complete");
    Ok(())
}

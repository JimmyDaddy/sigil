use super::*;

#[derive(Clone, Copy)]
enum PermissionOutcome {
    Deny,
    Cancel,
    Disconnect,
}

#[tokio::test]
async fn sdk_real_tool_approval_deny_preserves_workspace() -> Result<()> {
    run_permission_case(PermissionOutcome::Deny).await
}

#[tokio::test]
async fn sdk_real_tool_approval_cancel_unblocks_without_permission_response() -> Result<()> {
    run_permission_case(PermissionOutcome::Cancel).await
}

#[tokio::test]
async fn sdk_real_tool_approval_disconnect_joins_without_permission_response() -> Result<()> {
    run_permission_case(PermissionOutcome::Disconnect).await
}

async fn run_permission_case(outcome: PermissionOutcome) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().canonicalize()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let config = write_sdk_config(&workspace, listener.local_addr()?)?;
    let mut contents = std::fs::read_to_string(&config)?;
    contents.push_str("\n[permission]\nmode = \"manual\"\n");
    std::fs::write(&config, contents)?;
    sigil_kernel::RootConfig::load(&config)?;
    let (stop, mut stopped) = watch::channel(false);
    let provider = tokio::spawn(async move {
        let mut requested_write = false;
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
            if !title && !requested_write {
                requested_write = true;
                reply_with_write_tool(&mut socket).await?;
            } else {
                reply_from_fixture_provider(&mut socket).await?;
            }
        }
        Ok::<_, anyhow::Error>(requests)
    });
    let (agent_transport, client_transport) = agent_client_protocol::Channel::duplex();
    let mut server = tokio::spawn(async move { serve_transport(&config, agent_transport).await });
    let (permission_tx, mut permission_rx) = tokio::sync::mpsc::unbounded_channel();
    let client_workspace = workspace.clone();
    let client_result = tokio::time::timeout(
        Duration::from_secs(15),
        Client
            .builder()
            .on_receive_notification(
                async move |_notification: acp::SessionNotification, _connection| Ok(()),
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                async move |request: acp::RequestPermissionRequest, responder, _connection| {
                    permission_tx
                        .send((request, responder))
                        .map_err(|_| acp::Error::internal_error())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(client_transport, async move |client| {
                client
                    .send_request(acp::InitializeRequest::new(
                        agent_client_protocol::schema::ProtocolVersion::V1,
                    ))
                    .block_task()
                    .await?;
                let session = client
                    .send_request(acp::NewSessionRequest::new(client_workspace))
                    .block_task()
                    .await?;
                let invalid = client
                    .send_request(acp::PromptRequest::new(session.session_id.clone(), vec![]))
                    .block_task()
                    .await
                    .expect_err("empty prompt must remain invalid");
                if invalid.code != acp::ErrorCode::InvalidParams {
                    return Err(protocol_error(
                        "invalid input was misclassified on the wire",
                    ));
                }
                let response = client
                    .send_request(acp::PromptRequest::new(
                        session.session_id.clone(),
                        vec![acp::ContentBlock::Text(acp::TextContent::new(
                            "Create denied.txt with the supplied write tool.",
                        ))],
                    ))
                    .block_task();
                tokio::pin!(response);
                let (request, responder) = tokio::select! {
                    permission = permission_rx.recv() => permission
                        .ok_or_else(|| protocol_error("permission request missing"))?,
                    result = &mut response => return Err(protocol_error(format!(
                        "real broker ended before SDK request_permission: {result:?}"
                    ))),
                };
                let wire = serde_json::to_value(&request).map_err(protocol_error)?;
                if request.session_id != session.session_id
                    || wire["toolCall"]["toolCallId"] != "call_editor_approval"
                    || wire["toolCall"]["rawInput"]["path"] != "denied.txt"
                    || wire["_meta"]["sigil/approvalIdentity"].is_null()
                {
                    return Err(protocol_error("permission lost exact broker identity"));
                }
                let offered: Vec<_> = request
                    .options
                    .iter()
                    .map(|option| option.option_id.0.as_ref())
                    .collect();
                if offered != ["allow-once", "deny-once"] {
                    return Err(protocol_error("permission options changed"));
                }
                match outcome {
                    PermissionOutcome::Deny => {
                        responder.respond(acp::RequestPermissionResponse::new(
                            acp::RequestPermissionOutcome::Selected(
                                acp::SelectedPermissionOutcome::new("deny-once"),
                            ),
                        ))?;
                        let response = response.await?;
                        if response.stop_reason != acp::StopReason::EndTurn {
                            return Err(protocol_error("denied tool did not finish normally"));
                        }
                    }
                    PermissionOutcome::Cancel => {
                        client
                            .send_notification(acp::CancelNotification::new(session.session_id))?;
                        let response = response.await?;
                        if response.stop_reason != acp::StopReason::Cancelled {
                            return Err(protocol_error("approval cancellation was not confirmed"));
                        }
                        // The peer never answers: only the original run cancellation unblocks it.
                        drop(responder);
                    }
                    PermissionOutcome::Disconnect => {
                        // Closing the actual SDK transport must join the waiting run without a reply.
                        drop(responder);
                    }
                }
                Ok(())
            }),
    )
    .await;
    let server_result = tokio::time::timeout(Duration::from_secs(10), &mut server).await;
    if server_result.is_err() {
        server.abort();
        let _ = server.await;
    }
    stop.send_replace(true);
    let requests = provider
        .await
        .context("approval fixture provider joined")??;
    client_result.context("SDK approval deadline")??;
    server_result.context("SDK approval disconnect join deadline")???;
    assert!(!workspace.join("denied.txt").exists());
    if matches!(outcome, PermissionOutcome::Deny) {
        assert!(requests.iter().any(|request| {
            request["messages"].as_array().is_some_and(|messages| {
                messages.iter().any(|message| {
                    message["role"] == "tool"
                        && message["tool_call_id"] == "call_editor_approval"
                        && message["content"]
                            .as_str()
                            .is_some_and(|content| content.contains("Denied in the ACP client"))
                })
            })
        }));
    } else {
        let mut receipts = Vec::new();
        for entry in walkdir::WalkDir::new(workspace.join("state")) {
            let entry = entry?;
            if entry.file_name() == "records.jsonl" {
                for line in std::fs::read_to_string(entry.path())?.lines() {
                    let record: serde_json::Value = serde_json::from_str(line)?;
                    if record["event_type"] == "run_finalized"
                        && record["payload"]["record"] == "finalized"
                    {
                        receipts.push(record["payload"].clone());
                    }
                }
            }
        }
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0]["outcome"], "cancelled");
        assert_eq!(receipts[0]["cleanup_complete"], true);
        assert_eq!(receipts[0]["active_tasks"], 0);
        assert_eq!(receipts[0]["active_effects"], 0);
    }
    Ok(())
}

async fn reply_with_write_tool(socket: &mut tokio::net::TcpStream) -> Result<()> {
    let call = serde_json::json!({
        "id":"fixture", "object":"chat.completion.chunk", "model":"gpt-4.1",
        "choices":[{"index":0,"delta":{"tool_calls":[{
            "index":0,"id":"call_editor_approval","type":"function",
            "function":{"name":"write_file","arguments":"{\"path\":\"denied.txt\",\"content\":\"must remain absent\\n\"}"}
        }]},"finish_reason":null}]
    });
    let end = serde_json::json!({
        "id":"fixture", "object":"chat.completion.chunk", "model":"gpt-4.1",
        "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],
        "usage":{"prompt_tokens":20,"completion_tokens":5,"total_tokens":25}
    });
    let events = format!("data: {call}\n\ndata: {end}\n\ndata: [DONE]\n\n");
    socket
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{events}",
                events.len()
            )
            .as_bytes(),
        )
        .await?;
    Ok(())
}

//! Real production supervision must converge after stopping a manual file approval.

use super::*;

// Failed assertions must still notify the real supervisor before Tokio shuts down its
// executor. Otherwise an owned blocking approval wait can outlive the test future forever.
struct ProductionApprovalRunCleanup {
    driver: Arc<HttpProductionRunDriver>,
    session_id: String,
    run_id: String,
}

impl Drop for ProductionApprovalRunCleanup {
    fn drop(&mut self) {
        let _ = self.driver.cancel_run(HttpRunDriverCancel {
            session_id: self.session_id.clone(),
            run_id: self.run_id.clone(),
            reason: Some("approval fixture scope ended".to_owned()),
        });
    }
}

fn approval_fixture_run_diagnostics(
    driver: &HttpProductionRunDriver,
    session: &HttpSessionSnapshot,
    run_id: &str,
) -> String {
    match driver
        .event_bus
        .replay_run_after(&session.durable_session_scope_id, run_id, None)
    {
        Ok(events) => events
            .into_iter()
            .filter_map(|event| {
                Some(match event.run_event?.event {
                    PublicRunEventKind::RouteRecoveryRequired {
                        code, retryable, ..
                    } => format!("route_recovery={code:?}, retryable={retryable}"),
                    PublicRunEventKind::RunFailed { error } => {
                        format!("run_failed={}", error.chars().take(512).collect::<String>())
                    }
                    PublicRunEventKind::RunPaused { reason }
                    | PublicRunEventKind::RunBlocked { reason } => {
                        format!("stopped={}", reason.chars().take(512).collect::<String>())
                    }
                    PublicRunEventKind::RunAwaitingUserInput { .. } => {
                        "awaiting_user_input".to_owned()
                    }
                    PublicRunEventKind::ToolResult { result } => match result.status {
                        sigil_kernel::ToolResultStatus::Ok => "tool_result_ok".to_owned(),
                        sigil_kernel::ToolResultStatus::Error(error) => format!(
                            "tool_error={:?}; message={}",
                            error.kind,
                            error.message.chars().take(512).collect::<String>(),
                        ),
                    },
                    other => serde_json::to_value(other)
                        .ok()?
                        .get("type")?
                        .as_str()?
                        .to_owned(),
                })
            })
            .take(32)
            .collect::<Vec<_>>()
            .join("; "),
        Err(error) => format!("public replay failed: {error}"),
    }
}

async fn read_approval_fixture_provider_request(
    stream: &mut tokio::net::TcpStream,
) -> Result<serde_json::Value> {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0_u8; 16 * 1024];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count != 0, "fixture provider request ended before its body");
        bytes.extend_from_slice(&chunk[..count]);
        anyhow::ensure!(
            bytes.len() <= 2 * 1024 * 1024,
            "fixture provider request exceeds its bound"
        );
        let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let header = std::str::from_utf8(&bytes[..header_end])?;
        let length = header
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            })
            .context("bounded request content-length")?;
        if bytes.len() >= header_end + 4 + length {
            return Ok(serde_json::from_slice(
                &bytes[header_end + 4..header_end + 4 + length],
            )?);
        }
    }
}

fn approval_fixture_request_diagnostics(requests: &Arc<Mutex<Vec<serde_json::Value>>>) -> String {
    requests
        .lock()
        .expect("captured fixture requests")
        .iter()
        .map(|request| {
            let messages = request["messages"].as_array();
            let system = messages
                .and_then(|messages| messages.first())
                .and_then(|message| message["content"].as_str())
                .unwrap_or("");
            format!(
                "messages={}, tools={}, first_system={}",
                messages.map_or(0, Vec::len),
                request["tools"].as_array().map_or(0, Vec::len),
                system.chars().take(160).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn production_exec_approval_freezes_complete_safe_action() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let external = tempfile::tempdir()?;
    let config_path = fixture.path().join("sigil.toml");
    write_production_test_config(&config_path, ".");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    std::fs::write(
        &config_path,
        std::fs::read_to_string(&config_path)?
            .replace("http://127.0.0.1:1", &format!("http://{address}")),
    )?;
    let command = "PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests\n# Authorization: Bearer exec-preview-secret".to_owned();
    let args = serde_json::json!({ "command": command, "cwd": external.path(), "shell": "sh" });
    let provider = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("actual provider request");
        read_complete_provider_request(&mut stream)
            .await
            .expect("complete provider request");
        let chunk = serde_json::json!({"choices": [{"delta": {"tool_calls": [{
                "index": 0, "id": "frozen-exec-call", "type": "function",
                "function": {"name": "exec_command", "arguments": args.to_string()}
            }]}, "finish_reason": "tool_calls"}]});
        let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.expect("provider response");
        stream.shutdown().await.expect("provider complete");
    });
    let driver = approval_switch_driver(&fixture, "exec-preview")?;
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        fixture.path().join("commands-exec-preview.json"),
        32,
    )?))?;
    let session = registry.create_session(HttpSessionCreateRequest::default())?;
    let run = registry.start_run(
        &session.id,
        HttpRunStartRequest {
            prompt: "Review the exact command before execution".to_owned(),
            review_annotations: Vec::new(),
            image_attachments: Vec::new(),
            permission_mode: Some(HttpPermissionMode::Manual),
            model_ref: None,
            model_selection_binding: None,
            route_recovery_binding: None,
            reasoning_effort: None,
            reasoning_effort_binding: None,
            skill_binding: None,
            agent_binding: None,
            task_continuation: None,
        },
    )?;
    let _cleanup = ProductionApprovalRunCleanup {
        driver: Arc::clone(&driver),
        session_id: session.id.clone(),
        run_id: run.id.clone(),
    };
    let waiting = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let current = registry.get_run(&run.id)?;
            if current.status == HttpRunStatus::WaitingForApproval
                && current
                    .pending_approvals
                    .iter()
                    .any(|pending| pending.display.reconciliation_id.is_some())
            {
                break anyhow::Ok(current);
            }
            assert!(
                !current.status.is_terminal(),
                "real exec must reach approval: {current:?}; {}",
                approval_fixture_run_diagnostics(&driver, &session, &run.id),
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    let pending = waiting
        .pending_approvals
        .first()
        .context("real exec approval")?;
    assert_eq!(pending.tool_name, "exec_command");
    let records = JsonlSessionStore::new(&session.session_log_path)?.read_event_records_writer()?;
    let projection = PublicEventOutboxProjectionV1::from_records(&records)?;
    let source = projection
        .events_in_order()
        .into_iter()
        .find_map(|entry| {
            if let PublicRunEventKind::ApprovalRequested { call, preview, .. } = &entry.event.event
            {
                (call.id == pending.call_id).then(|| preview.clone())
            } else {
                None
            }
        })
        .flatten()
        .context("builtin must supply an actual frozen source, including external access")?;
    assert!(source.body.contains(&external.path().display().to_string()));
    assert!(source.body.contains("External directory access"));
    assert!(source.body.contains("Working directory"));
    assert!(source.body.contains("Shell: sh (posix)"));
    assert!(
        source.body.ends_with(&command),
        "the private frozen source must preserve the entire original action"
    );
    let protocol = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(event) = driver
                .event_bus
                .replay_run_after(&session.durable_session_scope_id, &run.id, None)?
                .into_iter()
                .find(|event| {
                    event
                        .approval_request
                        .as_ref()
                        .is_some_and(|guard| guard.call_id == pending.call_id)
                })
            {
                break anyhow::Ok(event);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("actual delivered HTTP approval")??;
    assert!(protocol.has_valid_approval_metadata());
    assert_eq!(pending.display.reconciliation_id, protocol.provisional_id);
    assert!(pending.display.reconciliation_id.is_some());
    let page = registry.conversation_display_page(&session.id, None, 50)?;
    let canonical_approval = page
        .items
        .iter()
        .find(|item| {
            matches!(&item.content,
            crate::HttpConversationDisplayContent::Approval { call_id, .. }
                if call_id == &pending.call_id)
        })
        .context("real canonical approval item")?;
    assert_eq!(canonical_approval.run_id.as_deref(), Some(run.id.as_str()));
    assert!(
        canonical_approval
            .reconciles
            .as_ref()
            .is_some_and(|identities| {
                identities
                    .iter()
                    .any(|identity| Some(identity) == pending.display.reconciliation_id.as_ref())
            }),
        "the actual HTTP snapshot must bind the actual canonical approval slot"
    );
    assert_eq!(
        protocol.approval_request.as_ref(),
        Some(pending),
        "HTTP replay and canonical snapshot must bind the same exact request/hash/review"
    );
    let PublicRunEventKind::ApprovalRequested {
        preview: Some(public_source),
        ..
    } = &protocol
        .run_event
        .as_ref()
        .context("actual HTTP public event")?
        .event
    else {
        anyhow::bail!("actual HTTP approval must carry the same frozen source's safe projection")
    };
    assert!(
        public_source
            .body
            .ends_with(&sigil_kernel::safe_persistence_text(&command))
    );
    assert!(
        !serde_json::to_string(&protocol)?.contains("exec-preview-secret"),
        "no credential value may reach the actual HTTP review"
    );
    assert_eq!(
        pending.display.preview,
        crate::sse::approval_preview_for_display(&source)
    );
    assert!(
        pending
            .display
            .preview
            .as_ref()
            .context("complete canonical exec review")?
            .body
            .contains("python3 -m unittest discover -s tests")
    );
    assert_eq!(
        registry.get_run(&run.id)?.pending_approvals,
        waiting.pending_approvals
    );
    for mismatch in [
        "run",
        "call",
        "request",
        "hash",
        "policy",
        "sequence",
        "anchor",
        "anchor_source",
    ] {
        let mut forged = protocol.clone();
        match mismatch {
            "run" => forged.run_event.as_mut().context("source")?.run_id = "another-run".to_owned(),
            "call" => {
                forged.approval_request.as_mut().context("guard")?.call_id =
                    "another-call".to_owned()
            }
            "request" => {
                forged
                    .approval_request
                    .as_mut()
                    .context("guard")?
                    .approval_request_id = "another-request".to_owned()
            }
            "hash" => {
                forged
                    .approval_request
                    .as_mut()
                    .context("guard")?
                    .tool_call_hash = "f".repeat(64)
            }
            "policy" => {
                forged
                    .approval_request
                    .as_mut()
                    .context("guard")?
                    .policy_version = "another-policy".to_owned()
            }
            "sequence" => {
                forged
                    .approval_request
                    .as_mut()
                    .context("guard")?
                    .display
                    .event_sequence += 1
            }
            "anchor" => {
                forged
                    .approval_request
                    .as_mut()
                    .context("guard")?
                    .display
                    .reconciliation_id = Some(format!("live-v1:{}", "f".repeat(64)))
            }
            "anchor_source" => {
                let wrong = Some(format!("live-v1:{}", "f".repeat(64)));
                forged.provisional_id = wrong.clone();
                forged
                    .approval_request
                    .as_mut()
                    .context("guard")?
                    .display
                    .reconciliation_id = wrong;
            }
            _ => unreachable!("bounded fixture variants"),
        }
        assert!(
            !registry.update_approval_publication(&run.id, &forged),
            "must reject {mismatch}"
        );
        assert_eq!(
            registry.get_run(&run.id)?.pending_approvals,
            waiting.pending_approvals
        );
    }
    assert!(
        std::fs::read_dir(external.path())?.next().is_none(),
        "approval preview must not run the command"
    );
    let cancel_registry = Arc::clone(&registry);
    let run_id = run.id.clone();
    tokio::task::spawn_blocking(move || cancel_registry.cancel_run(&run_id)).await??;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if registry.get_run(&run.id)?.status == HttpRunStatus::Cancelled
                && driver.active_run_count()? == 0
            {
                break anyhow::Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    provider.await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn production_manual_write_approval_cancel_seals_stream_and_releases_foreground() -> Result<()>
{
    manual_write_approval_cancel(true).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn production_manual_write_approval_cancel_seals_stream_and_releases_foreground_direct()
-> Result<()> {
    manual_write_approval_cancel(false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn production_manual_write_approval_switch_reuses_live_lease_and_allows_followup()
-> Result<()> {
    let fixture = tempfile::tempdir()?;
    let config_path = fixture.path().join("sigil.toml");
    write_production_test_config(&config_path, ".");
    let provider_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let provider_address = provider_listener.local_addr()?;
    let provider_requests = Arc::new(Mutex::new(Vec::new()));
    let captured_provider_requests = Arc::clone(&provider_requests);
    let (stop_provider, mut provider_stopped) = tokio::sync::oneshot::channel::<()>();
    std::fs::write(
        &config_path,
        std::fs::read_to_string(&config_path)?
            .replace("http://127.0.0.1:1", &format!("http://{provider_address}")),
    )?;
    let provider = tokio::spawn(async move {
        let mut index = 0;
        loop {
            let (mut stream, _) = tokio::select! {
                _ = &mut provider_stopped => break,
                accepted = provider_listener.accept() => accepted.expect("actual provider request"),
            };
            assert!(index < 8, "fixture provider requests must remain bounded");
            let request = read_approval_fixture_provider_request(&mut stream)
                .await
                .expect("complete actual request");
            captured_provider_requests
                .lock()
                .expect("fixture requests")
                .push(request);
            let chunk = if index == 0 {
                serde_json::json!({"choices": [{"delta": {"tool_calls": [{
                    "index": 0, "id": "switch-write-call", "type": "function",
                    "function": {"name": "write_file", "arguments": "{\"path\":\"approved-after-switch.txt\",\"content\":\"approved after returning\\n\"}"}
                }]}, "finish_reason": "tool_calls"}]})
            } else {
                serde_json::json!({"choices": [{"delta": {"content": "The requested operation completed."}, "finish_reason": "stop"}]})
            };
            let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.expect("actual response");
            stream.shutdown().await.expect("provider response complete");
            index += 1;
        }
    });
    let driver = approval_switch_driver(&fixture, "same-backend")?;
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        fixture.path().join("commands-switch-approval.json"),
        32,
    )?))?;
    let first = registry.create_session(HttpSessionCreateRequest::default())?;
    let request = |prompt: &str| HttpRunStartRequest {
        prompt: prompt.to_owned(),
        review_annotations: Vec::new(),
        image_attachments: Vec::new(),
        permission_mode: Some(HttpPermissionMode::Manual),
        model_ref: None,
        model_selection_binding: None,
        route_recovery_binding: None,
        reasoning_effort: None,
        reasoning_effort_binding: None,
        skill_binding: None,
        agent_binding: None,
        task_continuation: None,
    };
    let run = registry.start_run(&first.id, request("Create the requested file"))?;
    let _cleanup = ProductionApprovalRunCleanup {
        driver: Arc::clone(&driver),
        session_id: first.id.clone(),
        run_id: run.id.clone(),
    };
    let pending = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let current = registry.get_run(&run.id)?;
            if current.status == HttpRunStatus::WaitingForApproval
                && current
                    .pending_approvals
                    .iter()
                    .any(|approval| approval.display.reconciliation_id.is_some())
            {
                break anyhow::Ok(current);
            }
            assert!(
                !current.status.is_terminal(),
                "must reach real approval: {current:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    let approval = pending
        .pending_approvals
        .first()
        .context("actual frozen approval")?
        .clone();
    assert!(
        approval
            .display
            .preview
            .as_ref()
            .context("exact review")?
            .body
            .contains("approved after returning")
    );
    let original_lease = driver.acquire_exact_session_attachment(
        &first.durable_session_scope_id,
        Path::new(&first.session_log_path),
    )?;
    let original_generation = original_lease.generation().to_owned();
    let original_weak = Arc::downgrade(&original_lease);
    drop(original_lease);
    let second = registry.create_session(HttpSessionCreateRequest::default())?;
    assert_ne!(second.id, first.id);
    assert!(
        !driver
            .session_attachments
            .lock()
            .expect("selected attachment map")
            .contains_key(&first.durable_session_scope_id)
    );

    let entry = driver
        .options
        .session_lifecycle
        .as_ref()
        .context("real reopen catalog")?
        .catalog()?
        .entries
        .into_iter()
        .find(|entry| entry.session_id.as_deref() == Some(first.durable_session_scope_id.as_str()))
        .context("actual first session catalog identity")?;
    let reopened = registry.open_session(crate::HttpSessionOpenRequest {
        session_ref: entry.session_ref.as_path().to_string_lossy().into_owned(),
        session_id: first.durable_session_scope_id.clone(),
        label: Some("Returned session".to_owned()),
        recovery_binding: None,
    })?;
    assert_eq!(reopened.id, first.id);
    assert!(reopened.route_recovery.is_none());
    assert!(driver.run_context_view(&reopened)?.route_recovery.is_none());
    let returned_lease = driver.acquire_exact_session_attachment(
        &first.durable_session_scope_id,
        Path::new(&first.session_log_path),
    )?;
    assert_eq!(returned_lease.generation(), original_generation);
    assert!(Arc::ptr_eq(
        &returned_lease,
        &original_weak
            .upgrade()
            .context("actual live supervisor lease")?
    ));
    drop(returned_lease);
    assert_eq!(
        registry.get_run(&run.id)?.pending_approvals,
        vec![approval.clone()]
    );
    assert_eq!(
        registry
            .session_continuity(&first.id)?
            .foreground_owner
            .context("same live owner")?
            .run_id,
        run.id
    );
    assert!(!fixture.path().join("approved-after-switch.txt").exists());

    // Another driver has no capability in its own weak index. A real OS lease must still reject
    // that controller while the first driver's actual supervisor owns this attachment.
    let foreign_fixture = tempfile::tempdir()?;
    let foreign = production_queue_driver(&foreign_fixture, "foreign-attachment");
    assert!(matches!(
        foreign.acquire_exact_session_attachment(
            &first.durable_session_scope_id,
            Path::new(&first.session_log_path)
        ),
        Err(HttpRunAdmissionError::SessionAlreadyActive { .. })
    ));
    assert!(matches!(
        driver.acquire_exact_session_attachment(
            &first.durable_session_scope_id,
            Path::new(&second.session_log_path)
        ),
        Err(HttpRunAdmissionError::Unavailable)
    ));

    registry.submit_approval_command(
        &run.id,
        &approval.call_id,
        crate::HttpCommandEnvelope::new(
            "approve-returned-file",
            "same-backend-client",
            &first.id,
            crate::HttpApprovalDecisionRequest {
                approval_request_id: approval.approval_request_id,
                tool_call_hash: approval.tool_call_hash,
                policy_version: approval.policy_version,
                expires_at_ms: approval.expires_at_ms,
                decision: crate::HttpApprovalDecision::Approve,
                family_pattern: None,
                reason: None,
            },
        ),
    )?;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if registry.get_run(&run.id)?.status.is_terminal() && driver.active_run_count()? == 0 {
                break anyhow::Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("actual approved execution must finish")??;
    assert_eq!(registry.get_run(&run.id)?.status, HttpRunStatus::Finished);
    assert_eq!(
        std::fs::read_to_string(fixture.path().join("approved-after-switch.txt"))?,
        "approved after returning\n"
    );
    assert!(
        registry
            .session_continuity(&first.id)?
            .foreground_owner
            .is_none()
    );
    assert!(
        original_weak.upgrade().is_some(),
        "the selected idle controller still owns its actual lease"
    );
    // Completion ends the execution owner; a selected idle UI controller still owns the
    // attachment. A real session switch, rather than a cache mutation, releases that owner.
    let selected_after_completion = registry.create_session(HttpSessionCreateRequest::default())?;
    assert!(
        driver
            .session_attachments
            .lock()
            .expect("selected attachments")
            .contains_key(&selected_after_completion.durable_session_scope_id)
    );
    // Reproduce the old release callback after the user already switched away. An empty
    // background queue inspection must neither reselect A nor retain its idle attachment.
    assert!(driver.next_queued_run_admission(&first)?.is_none());
    {
        let selected = driver
            .session_attachments
            .lock()
            .expect("selected attachments");
        assert!(selected.contains_key(&selected_after_completion.durable_session_scope_id));
        assert!(!selected.contains_key(&first.durable_session_scope_id));
    }
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if original_weak.upgrade().is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("switching away must release the actual idle attachment")?;
    let foreign_lease = foreign.acquire_exact_session_attachment(
        &first.durable_session_scope_id,
        Path::new(&first.session_log_path),
    )?;
    assert_ne!(foreign_lease.generation(), original_generation);
    assert!(matches!(
        driver.acquire_exact_session_attachment(
            &first.durable_session_scope_id,
            Path::new(&first.session_log_path)
        ),
        Err(HttpRunAdmissionError::SessionAlreadyActive { .. })
    ));
    assert!(
        !driver
            .session_attachment_index
            .lock()
            .expect("weak index")
            .contains_key(&first.durable_session_scope_id)
    );
    drop(foreign_lease);
    let acquired_again = driver.acquire_exact_session_attachment(
        &first.durable_session_scope_id,
        Path::new(&first.session_log_path),
    )?;
    assert_ne!(acquired_again.generation(), original_generation);
    drop(acquired_again);
    let next = registry.start_run(&first.id, request("Continue with the next request"))?;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if registry.get_run(&next.id)?.status.is_terminal() && driver.active_run_count()? == 0 {
                break anyhow::Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert_eq!(
        registry.get_run(&next.id)?.status,
        HttpRunStatus::Finished,
        "new prompt must remain usable: {}; provider requests: {}",
        approval_fixture_run_diagnostics(&driver, &first, &next.id),
        approval_fixture_request_diagnostics(&provider_requests),
    );
    assert_eq!(
        provider_requests
            .lock()
            .expect("fixture requests")
            .iter()
            .filter(|request| request["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty()))
            .count(),
        3,
        "the real tool-call, post-tool answer and followup each need exactly one foreground provider request"
    );
    let _ = stop_provider.send(());
    tokio::time::timeout(Duration::from_secs(5), provider).await??;
    Ok(())
}

fn approval_switch_driver(
    fixture: &tempfile::TempDir,
    suffix: &str,
) -> Result<Arc<HttpProductionRunDriver>> {
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(
        32,
        Arc::new(HttpDurableProtocolJournal::open(
            fixture.path().join(format!("protocol-{suffix}.json")),
            32,
        )?),
    ));
    let disclosures = Arc::new(HttpDurableEgressDisclosureJournal::open(
        fixture.path().join(format!("disclosures-{suffix}.json")),
        32,
    )?);
    let lifecycle = sigil_runtime::LocalSessionLifecycleService::new(
        "approval-switch",
        fixture.path().join("sessions"),
        fixture.path().join("session-exports"),
    );
    Ok(Arc::new(HttpProductionRunDriver::new(
        HttpProductionRunDriverOptions::new(fixture.path().join("sigil.toml"), fixture.path())
            .with_session_lifecycle(lifecycle),
        disclosures,
        event_bus,
        tokio::runtime::Handle::current(),
    )?))
}

async fn manual_write_approval_cancel(application_cancel: bool) -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let provider_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let provider_address = provider_listener.local_addr()?;
    let provider = tokio::spawn(async move {
        for index in 0..2 {
            let (mut stream, _) = provider_listener
                .accept()
                .await
                .expect("actual provider request");
            read_complete_provider_request(&mut stream)
                .await
                .expect("complete actual request");
            let chunk = if index == 0 {
                serde_json::json!({"choices": [{"delta": {"tool_calls": [{
                    "index": 0, "id": "cancel-write-call", "type": "function",
                    "function": {"name": "write_file", "arguments": "{\"path\":\"unapproved.txt\",\"content\":\"must not be written\\n\"}"}
                }]}, "finish_reason": "tool_calls"}]})
            } else {
                serde_json::json!({"choices": [{"delta": {"content": "The next request is available."}, "finish_reason": "stop"}]})
            };
            let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.expect("actual provider response");
            stream.shutdown().await.expect("provider response complete");
        }
    });
    let driver = production_queue_driver(&fixture, "approval-cancel");
    let config_path = fixture.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        std::fs::read_to_string(&config_path)?
            .replace("http://127.0.0.1:1", &format!("http://{provider_address}")),
    )?;
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        fixture.path().join("commands-approval-cancel.json"),
        32,
    )?))?;
    let session = registry.create_session(HttpSessionCreateRequest::default())?;
    let request = |prompt: &str| HttpRunStartRequest {
        prompt: prompt.to_owned(),
        review_annotations: Vec::new(),
        image_attachments: Vec::new(),
        permission_mode: Some(HttpPermissionMode::Manual),
        model_ref: None,
        model_selection_binding: None,
        route_recovery_binding: None,
        reasoning_effort: None,
        reasoning_effort_binding: None,
        skill_binding: None,
        agent_binding: None,
        task_continuation: None,
    };
    let run = registry.start_run(&session.id, request("Create the requested file"))?;
    let pending = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let current = registry.get_run(&run.id)?;
            if current.status == HttpRunStatus::WaitingForApproval {
                break anyhow::Ok(current);
            }
            assert!(
                !current.status.is_terminal(),
                "must reach real manual approval: {current:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert_eq!(pending.pending_approvals.len(), 1);
    assert_eq!(pending.pending_approvals[0].tool_name, "write_file");
    assert!(
        pending.pending_approvals[0]
            .display
            .preview
            .as_ref()
            .context("actual production canonical approval must retain its frozen review")?
            .body
            .contains("must not be written")
    );
    assert!(!fixture.path().join("unapproved.txt").exists());
    assert!(
        !reconcile_failed_http_supervisor_terminal(
            Arc::clone(&registry),
            Arc::clone(&driver.event_bus),
            session.clone(),
            run.id.clone(),
            "no-joined-execution".to_owned(),
        )
        .await?,
        "an active approval has no proven cleanup/terminal and cannot be cleared as success"
    );
    assert_eq!(
        registry.get_run(&run.id)?.status,
        HttpRunStatus::WaitingForApproval
    );
    assert!(
        registry
            .session_continuity(&session.id)?
            .foreground_owner
            .is_some()
    );
    assert!(
        driver
            .event_bus
            .run_stream_accepts_events(&session.durable_session_scope_id, &run.id)?
    );
    if application_cancel {
        let client = registry.application_client(&session.id, "approval-stop-client")?;
        let slot = {
            let active = driver
                .active_runs
                .lock()
                .expect("real active owner")
                .get(&run.id)
                .cloned()
                .context("manual approval owner")?;
            active
                .broker
                .pending
                .lock()
                .expect("real approval slots")
                .get(&pending.pending_approvals[0].approval_request_id)
                .cloned()
                .context("actual pending approval slot")?
        };
        // A real owner mutex provides a deterministic barrier after cancellation is requested,
        // before the approval wait can join. It changes no production branch or timeout.
        let (held, holding) = tokio::sync::oneshot::channel();
        let (release, released) = std_mpsc::channel();
        let approval_wait = std::thread::spawn(move || {
            let _wait = slot.state.lock().expect("hold actual approval wait");
            let _ = held.send(());
            released
                .recv_timeout(Duration::from_secs(10))
                .expect("bounded approval barrier");
        });
        holding.await?;
        let cancellation = tokio::task::spawn_blocking(move || {
            client.execute_without_refresh(
                "approval-stop-command",
                None,
                ApplicationCommand::Run(sigil_application::RunCommand::Cancel {
                    binding: run.id,
                    reason: None,
                }),
            )
        });
        let requested_scope = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let records = JsonlSessionStore::new(&session.session_log_path)?
                    .read_event_records_writer()?;
                if let Some(request) = records.iter().find_map(|record| {
                    serde_json::from_value::<sigil_kernel::RunCancellationRequestedEntry>(
                        record.stored_event().payload.clone(),
                    )
                    .ok()
                }) {
                    break anyhow::Ok(request.run_scope_id);
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .context("actual cancellation request should reach the held approval")??;
        assert!(
            !reconcile_failed_http_supervisor_terminal(
                Arc::clone(&registry),
                Arc::clone(&driver.event_bus),
                session.clone(),
                pending.id.clone(),
                requested_scope,
            )
            .await?,
            "a requested cancellation without actual cleanup and terminal remains unresolved"
        );
        assert!(
            registry
                .session_continuity(&session.id)?
                .foreground_owner
                .is_some()
        );
        assert!(
            driver
                .event_bus
                .run_stream_accepts_events(&session.durable_session_scope_id, &pending.id)?
        );
        release.send(())?;
        approval_wait
            .join()
            .expect("approval barrier should release");
        let cancellation = cancellation.await??;
        assert!(
            !matches!(cancellation, ApplicationCommandReceipt::Rejected(_)),
            "real stop must reach the owner: {cancellation:?}"
        );
    } else {
        let registry = Arc::clone(&registry);
        tokio::task::spawn_blocking(move || registry.cancel_run(&run.id)).await??;
    }
    let run_id = pending.id;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let current = registry.get_run(&run_id)?;
            if current.status == HttpRunStatus::Cancelled && driver.active_run_count()? == 0 {
                break anyhow::Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .with_context(|| format!("approval cancellation must converge to an actual released terminal: status={:?}, active={:?}", registry.get_run(&run_id).map(|run| run.status), driver.active_run_count()))??;
    assert_eq!(registry.get_run(&run_id)?.status, HttpRunStatus::Cancelled);
    assert!(
        !driver
            .event_bus
            .run_stream_accepts_events(&session.durable_session_scope_id, &run_id)?
    );
    assert!(
        registry
            .session_continuity(&session.id)?
            .foreground_owner
            .is_none()
    );
    assert!(
        !fixture.path().join("unapproved.txt").exists(),
        "cancellation cannot approve a pending write"
    );
    let records = JsonlSessionStore::new(&session.session_log_path)?.read_event_records_writer()?;
    let projection = PublicEventOutboxProjectionV1::from_records(&records)?;
    let actual_cleanup = records
        .iter()
        .find_map(|record| {
            serde_json::from_value::<sigil_kernel::RunCancellationFinalizedEntry>(
                record.stored_event().payload.clone(),
            )
            .ok()
        })
        .context("the actual production cancellation must record its owner's scope")?;
    assert_eq!(
        actual_cleanup.outcome,
        sigil_kernel::RunCancellationTerminalOutcome::Cancelled
    );
    assert!(actual_cleanup.cleanup_complete);
    assert_eq!(
        (actual_cleanup.active_effects, actual_cleanup.active_tasks),
        (0, 0)
    );
    assert!(projection.pending_for_adapter("http").is_empty());
    let cancelled = projection
        .events_in_order()
        .into_iter()
        .filter(|entry| {
            entry.run_id == run_id && matches!(entry.event.event, PublicRunEventKind::RunCancelled)
        })
        .count();
    assert_eq!(
        cancelled, 1,
        "the exact cancelled terminal must be committed and delivered once"
    );

    // Exercise the same error-recovery entry as the watcher with a lost protocol-close
    // projection. The source terminal came from the actual production cancellation above.
    let source_before = std::fs::read(&session.session_log_path)?;
    let recovered_bus = Arc::new(HttpLiveEventBus::with_durable_journal(
        32,
        Arc::new(HttpDurableProtocolJournal::open(
            fixture.path().join("failed-supervisor-replay.json"),
            32,
        )?),
    ));
    for entry in projection
        .events_in_order()
        .into_iter()
        .filter(|entry| entry.run_id == run_id)
    {
        recovered_bus.publish_run_event_with_stream_continuation(entry.event.clone())?;
    }
    assert!(recovered_bus.run_stream_accepts_events(&session.durable_session_scope_id, &run_id)?);
    assert!(
        !reconcile_failed_http_supervisor_terminal(
            Arc::clone(&registry),
            Arc::clone(&recovered_bus),
            session.clone(),
            run_id.clone(),
            "another-execution-scope".to_owned(),
        )
        .await?,
        "another execution's cleanup cannot prove this joined owner is clean"
    );
    assert!(recovered_bus.run_stream_accepts_events(&session.durable_session_scope_id, &run_id)?);
    assert!(
        reconcile_failed_http_supervisor_terminal(
            Arc::clone(&registry),
            Arc::clone(&recovered_bus),
            session.clone(),
            run_id.clone(),
            actual_cleanup.run_scope_id.clone(),
        )
        .await?
    );
    assert!(!recovered_bus.run_stream_accepts_events(&session.durable_session_scope_id, &run_id)?);
    assert_eq!(registry.get_run(&run_id)?.status, HttpRunStatus::Cancelled);
    assert!(
        registry
            .session_continuity(&session.id)?
            .foreground_owner
            .is_none()
    );
    assert_eq!(
        std::fs::read(&session.session_log_path)?,
        source_before,
        "recovery cannot replace the original terminal or its receipt"
    );
    if application_cancel {
        incomplete_cleanup_terminals_keep_foreground(
            &fixture,
            &records,
            &session,
            &run_id,
            &actual_cleanup,
        )
        .await?;
    }

    let next = registry.start_run(&session.id, request("Continue with the next request"))?;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if registry.get_run(&next.id)?.status.is_terminal() && driver.active_run_count()? == 0 {
                break anyhow::Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("new prompt must reach its own actual terminal")??;
    assert_eq!(registry.get_run(&next.id)?.status, HttpRunStatus::Finished);
    provider.await?;
    Ok(())
}

async fn incomplete_cleanup_terminals_keep_foreground(
    fixture: &tempfile::TempDir,
    actual_records: &[sigil_kernel::SessionStreamRecord],
    actual_session: &HttpSessionSnapshot,
    run_id: &str,
    actual_cleanup: &sigil_kernel::RunCancellationFinalizedEntry,
) -> Result<()> {
    let cutoff = actual_records
        .iter()
        .position(|record| {
            serde_json::from_value::<sigil_kernel::RunCancellationFinalizedEntry>(
                record.stored_event().payload.clone(),
            )
            .is_ok_and(|cleanup| cleanup.request_id == actual_cleanup.request_id)
        })
        .context("real cancellation cleanup boundary")?;
    let mut prefix = Vec::new();
    for record in &actual_records[..cutoff] {
        serde_json::to_writer(&mut prefix, record.stored_event())?;
        prefix.push(b'\n');
    }
    use sigil_kernel::ConversationRunTerminalStatusV1 as Status;
    for (label, status, event, cleanup_complete, active_effects, active_tasks) in [
        (
            "interrupted",
            Status::Interrupted,
            PublicRunEventKind::RunInterrupted {
                reason: "cleanup remains unresolved".to_owned(),
            },
            false,
            1,
            1,
        ),
        (
            "failed",
            Status::Failed,
            PublicRunEventKind::RunFailed {
                error: "cleanup remains unresolved".to_owned(),
            },
            false,
            1,
            1,
        ),
        // Even a Cancelled public pair cannot replace missing or contradictory cleanup proof.
        (
            "cancelled-incomplete",
            Status::Cancelled,
            PublicRunEventKind::RunCancelled,
            false,
            0,
            0,
        ),
        (
            "cancelled-active-effect",
            Status::Cancelled,
            PublicRunEventKind::RunCancelled,
            true,
            1,
            0,
        ),
        (
            "cancelled-active-task",
            Status::Cancelled,
            PublicRunEventKind::RunCancelled,
            true,
            0,
            1,
        ),
    ] {
        // Preserve the actual production control's scope and valid envelope prefix. Only this
        // independent copy receives a typed alternative terminal through the real durable writer.
        let path = fixture.path().join(format!("incomplete-{label}.jsonl"));
        std::fs::write(&path, &prefix)?;
        let store = JsonlSessionStore::new(&path)?;
        let session =
            sigil_kernel::Session::new("cleanup-fixture", "model").with_store(store.clone());
        let mut cleanup = actual_cleanup.clone();
        cleanup.outcome = if status == Status::Cancelled {
            sigil_kernel::RunCancellationTerminalOutcome::Cancelled
        } else {
            sigil_kernel::RunCancellationTerminalOutcome::Interrupted
        };
        cleanup.cleanup_complete = cleanup_complete;
        cleanup.active_effects = active_effects;
        cleanup.active_tasks = active_tasks;
        assert!(
            session
                .run_cancellation_recorder()?
                .append_finalized(&cleanup)?
        );
        let existing =
            PublicEventOutboxProjectionV1::from_records(&store.read_event_records_writer()?)?;
        let sequence = existing
            .events_in_order()
            .into_iter()
            .filter(|entry| entry.run_id == run_id)
            .map(|entry| entry.sequence)
            .max()
            .unwrap_or(0)
            + 1;
        let event = PublicRunEvent::new(
            &actual_session.durable_session_scope_id,
            run_id,
            sequence,
            event,
        );
        let outbox = sigil_kernel::PublicEventOutboxEntryV1 {
            schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: format!(
                "application-public:{}:{run_id}:{sequence}",
                actual_session.durable_session_scope_id
            ),
            domain_event_id: format!("application-domain:{run_id}:{sequence}"),
            run_id: run_id.to_owned(),
            sequence,
            payload_digest: sigil_kernel::stable_event_hash(&serde_json::to_vec(&event)?),
            event,
        };
        let terminal = sigil_kernel::ConversationRunFinalizedEntryV1::new(
            run_id,
            status,
            None,
            Some("cleanup remains unresolved"),
            current_unix_time_ms(),
            &sigil_kernel::SecretRedactor::empty(),
        )?;
        session
            .conversation_run_lifecycle_recorder()?
            .append_finalized_with_outbox(&terminal, &outbox)?;
        let registry = Arc::new(HttpSessionRunRegistry::new(Arc::new(
            ApprovalCancellationProjectionDriver(HttpSessionBinding {
                session_scope_id: actual_session.durable_session_scope_id.clone(),
                session_log_path: canonical_http_session_path(&path)?.display().to_string(),
                route_transition: None,
                route_recovery: None,
            }),
        )));
        let registered_session = registry.create_session(HttpSessionCreateRequest::default())?;
        let mutation =
            registry.reserve_durable_session_mutation(&actual_session.durable_session_scope_id)?;
        registry.register_supervised_session_run(
            &registered_session.id,
            run_id,
            HttpPermissionMode::Manual,
            "unresolved cleanup fixture",
        )?;
        drop(mutation);
        registry.record_run_execution_uncertain(run_id)?;
        let bus = Arc::new(HttpLiveEventBus::new(32));
        let records = store.read_event_records_writer()?;
        let projection = PublicEventOutboxProjectionV1::from_records(&records)?;
        for entry in projection
            .events_in_order()
            .into_iter()
            .filter(|entry| entry.run_id == run_id)
        {
            bus.publish_run_event_with_stream_continuation(entry.event.clone())?;
        }
        let bytes_before = std::fs::read(&path)?;
        let owner_before = registry
            .session_continuity(&registered_session.id)?
            .foreground_owner;
        assert!(owner_before.is_some());
        assert!(
            !reconcile_failed_http_supervisor_terminal(
                Arc::clone(&registry),
                Arc::clone(&bus),
                registered_session.clone(),
                run_id.to_owned(),
                actual_cleanup.run_scope_id.clone()
            )
            .await?,
            "{label}"
        );
        assert_eq!(
            registry.get_run(run_id)?.status,
            HttpRunStatus::ExecutionUncertain,
            "{label}"
        );
        assert_eq!(
            registry
                .session_continuity(&registered_session.id)?
                .foreground_owner,
            owner_before,
            "{label}"
        );
        assert!(
            bus.run_stream_accepts_events(&actual_session.durable_session_scope_id, run_id)?,
            "{label}"
        );
        assert_eq!(
            std::fs::read(&path)?,
            bytes_before,
            "{label}: no receipts or replacement terminal"
        );
    }
    Ok(())
}

struct ApprovalCancellationProjectionDriver(HttpSessionBinding);

impl HttpRunDriver for ApprovalCancellationProjectionDriver {
    fn bind_session(
        &self,
        _session_id: &str,
        _model_ref: Option<&crate::HttpProviderModelRef>,
    ) -> Result<HttpSessionBinding, HttpRunDriverError> {
        Ok(self.0.clone())
    }
    fn session_frontier(
        &self,
        session: &HttpSessionSnapshot,
    ) -> Result<crate::HttpDurableSessionFrontier, HttpRunDriverError> {
        let frontier = application_session_frontier_view(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
        )
        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
        Ok(crate::HttpDurableSessionFrontier {
            through_stream_sequence: frontier.through_stream_sequence,
        })
    }
    fn start_run(&self, _start: HttpRunDriverStart) -> Result<(), HttpRunDriverError> {
        Ok(())
    }
    fn cancel_run(&self, _cancel: crate::HttpRunDriverCancel) -> Result<(), HttpRunDriverError> {
        Ok(())
    }
    fn submit_approval(&self, _approval: HttpRunDriverApproval) -> Result<(), HttpRunDriverError> {
        Ok(())
    }
}

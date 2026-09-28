//! Actual authenticated application requests retain the same K/F across interrupted fork writes.
use super::*;
use crate::{HttpLocalServer, HttpServerConfig};
use futures::FutureExt as _;
use sigil_kernel::session::SessionWriterFault;
use sigil_runtime::managed_storage_writer::StorageWriterChannelV1;

async fn post_fork(
    address: std::net::SocketAddr,
    session_id: &str,
    command_id: &str,
    command: &ApplicationCommand,
) -> Result<ApplicationCommandReceipt> {
    let body = serde_json::json!({"command_id":command_id,"command":command}).to_string();
    let mut stream = tokio::net::TcpStream::connect(address).await?;
    stream.write_all(format!(
        "POST /sessions/{session_id}/application/commands HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer fork-recovery\r\nx-sigil-application-client-id: fork-recovery-client\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await?;
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(15), stream.read_to_end(&mut response)).await??;
    let split = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .context("response headers")?;
    let head = std::str::from_utf8(&response[..split])?;
    let body: serde_json::Value = serde_json::from_slice(&response[split + 4..])?;
    anyhow::ensure!(
        head.split_whitespace().nth(1) == Some("200"),
        "HTTP response {head}: {body}"
    );
    Ok(serde_json::from_value(body)?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_http_fork_recovers_partial_and_published_child_with_same_request() -> Result<()> {
    for fault in [
        SessionWriterFault::BeforeWrite,
        SessionWriterFault::PartialFirstRecord,
        SessionWriterFault::PartialSecondRecord,
        SessionWriterFault::BeforeSync,
    ] {
        let temp = tempfile::tempdir()?;
        let config_path = temp.path().join("sigil.toml");
        write_production_test_config(&config_path, ".");
        let sessions = temp.path().join("sessions");
        std::fs::create_dir(&sessions)?;
        let driver = Arc::new(HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(&config_path, temp.path()).with_session_lifecycle(
                LocalSessionLifecycleService::new(
                    "fork-recovery",
                    sessions,
                    temp.path().join("exports"),
                ),
            ),
            Arc::new(HttpDurableEgressDisclosureJournal::open(
                temp.path().join("disclosures.json"),
                16,
            )?),
            Arc::new(HttpLiveEventBus::with_durable_journal(
                16,
                Arc::new(HttpDurableProtocolJournal::open(
                    temp.path().join("protocol.json"),
                    16,
                )?),
            )),
            tokio::runtime::Handle::current(),
        )?);
        let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
            temp.path().join("commands.json"),
            16,
        )?))?;
        let source = registry.create_session(HttpSessionCreateRequest::default())?;
        let owner = driver.application_operation_owner(&source)?;
        let mut session = owner.owner.attach_for_control()?;
        session.append_user_message(sigil_kernel::ModelMessage::user("inspect the original"))?;
        let answer =
            sigil_kernel::ModelMessage::assistant(Some("original answer".into()), Vec::new());
        session.append_assistant_message(answer.clone())?;
        session.append_durable_event(sigil_kernel::DurableEventType::RunFinalized,
            sigil_kernel::EventClass::Critical, serde_json::json!({"run_status":"completed",
                "terminal_reason":"final_answer","final_message_id":answer.id,"tool_calls":0,"error":null}))?;
        drop(session);
        let point = registry
            .conversation_recovery(&source.id)?
            .fork_points
            .remove(0);
        let (_, route) = production_test_model_route(&temp);
        let command = ApplicationCommand::Conversation(ConversationCommand::Recovery {
            action: ApplicationRecoveryAction::ForkConversation {
                source_turn_digest: SafeText::new(point.source_turn_digest.clone())?,
                connection_id: SafeText::new(route.model_ref.connection_id.as_str())?,
                model_id: SafeText::new(route.model_ref.model_id.clone())?,
                request_id: None,
            },
        });
        let client = registry.application_client(&source.id, "fork-recovery-client")?;
        client.refresh()?;
        let request = crate::application_bridge::frontier_tests::prepare_http_request(
            &client,
            "same-fork-request",
            command.clone(),
        )?;
        let binding =
            sigil_runtime::application_operation_owner::application_operation_binding(&request)?
                .context("fork binding")?;
        let key = format!(
            "session-fork-{}",
            sigil_kernel::stable_event_uuid(
                "sigil-runtime-conversation-fork-path",
                &format!(
                    "{}:{}:{}",
                    source.durable_session_scope_id, point.source_turn_digest, binding.operation_id
                )
            )
        );
        let writer = &driver
            .services
            .authority_composition()
            .context("actual authority")?
            .storage_writer;
        let source_ref = driver.catalog_reference_for_session(&source)?;
        let config = RootConfig::load(&config_path)?;
        let missing_path =
            writer.managed_named_leaf_path(StorageWriterChannelV1::SessionLog, &key)?;
        assert!(!missing_path.exists());
        assert!(
            driver
                .session_lifecycle()
                .context("lifecycle")?
                .recover_existing_fork_session_at_turn(
                    &source_ref,
                    &source.durable_session_scope_id,
                    &point.source_turn_digest,
                    &binding.operation_id,
                    &config,
                    &route.model_ref
                )?
                .is_none()
        );
        assert!(
            !missing_path.exists(),
            "existing-only recovery must not allocate a namespace"
        );
        assert!(
            driver
                .query_application_operation(&source, &binding)
                .is_err(),
            "an unprepared binding cannot recover or create a branch"
        );
        // Allocate and settle the empty namespace through the real authority before obtaining
        // its test-only coordinator. No record, identity or copy is seeded by this fixture.
        let allocation = writer.acquire_named(StorageWriterChannelV1::SessionLog, &key)?;
        let destination_path = allocation.path().join("records.jsonl");
        writer.finalize(allocation)?;
        let destination = JsonlSessionStore::new(&destination_path)?;
        assert!(
            !destination_path.exists(),
            "fault setup must not publish a child"
        );
        destination.inject_writer_fault(fault)?;
        assert_eq!(destination.observed_writer_fault()?, None);
        let server = HttpLocalServer::bind(
            HttpServerConfig::default(),
            Some("fork-recovery"),
            Arc::clone(&registry),
        )
        .await?;
        let address = server.local_addr()?;
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(server.serve_until_shutdown(async move {
            let _ = shutdown_rx.await;
        }));
        let result = std::panic::AssertUnwindSafe(async {
            let first = post_fork(address, &source.id, "same-fork-request", &command).await?;
            let actual_prepared = owner.owner.read_handle().read_event_records()?
                .into_iter()
                .filter_map(|record| match record.session_log_entry() {
                    Ok(Some(SessionLogEntry::Control(ControlEntry::ApplicationOperationPreparedV1(binding)))) => Some(binding),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(actual_prepared.as_slice(), std::slice::from_ref(&binding),
                "fixture must derive the actual HTTP application K/F before selecting its destination");
            assert_eq!(destination.observed_writer_fault()?, Some(fault),
                "the injected fault must trigger in the actual child writer");
            let expected_ref = format!("{}.jsonl", missing_path.file_name().context("physical key")?.to_string_lossy());
            match &first {
                ApplicationCommandReceipt::Settled(receipt) => {
                    let Some(sigil_application::ApplicationCommandOutcome::Recovery(
                        sigil_application::ApplicationRecoveryOutcome::Fork { session_ref, .. }
                    )) = receipt.outcome.as_deref() else {
                        anyhow::bail!("the first settled receipt has no exact fork outcome: {first:?}");
                    };
                    assert_eq!(session_ref.as_str(), expected_ref,
                        "the recovered HTTP child differs from the faulted namespace");
                }
                ApplicationCommandReceipt::Uncertain(_) => {}
                other => anyhow::bail!("injected {fault:?} returned an invalid first receipt: {other:?}"),
            }
            let bytes = std::fs::read(&destination_path)?;
            let source_records = owner.owner.read_handle().read_event_records()?;
            assert!(source_records.iter().filter(|record| matches!(record.session_log_entry(),
                Ok(Some(SessionLogEntry::Control(ControlEntry::ConversationForkCommittedV1(_)))))).count() <= 1,
                "one fork request cannot commit more than one source marker");
            assert!(source_records.iter().any(|record| matches!(record.session_log_entry(),
                Ok(Some(SessionLogEntry::Control(ControlEntry::ApplicationOperationPreparedV1(ref prepared))))
                    if prepared == &binding)));
            let mut different_payload = command.clone();
            if let ApplicationCommand::Conversation(ConversationCommand::Recovery {
                action: ApplicationRecoveryAction::ForkConversation { connection_id, .. },
            }) = &mut different_payload {
                *connection_id = SafeText::new("missing-fork-connection")?;
            }
            let conflict = post_fork(address, &source.id, "same-fork-request", &different_payload).await?;
            assert!(matches!(conflict, ApplicationCommandReceipt::PayloadConflict(_)),
                "the original K must reject a changed F: {conflict:?}");
            assert_eq!(std::fs::read(&destination_path)?, bytes,
                "payload conflict cannot repair or replace the original child");
            let replay = post_fork(address, &source.id, "same-fork-request", &command).await?;
            let ApplicationCommandReceipt::Replayed(receipt) = &replay else {
                anyhow::bail!("same K/F did not recover child: {replay:?}");
            };
            anyhow::ensure!(matches!(receipt.outcome.as_deref(), Some(sigil_application::ApplicationCommandOutcome::Recovery(
                sigil_application::ApplicationRecoveryOutcome::Fork { .. }))), "exact fork outcome missing");
            if let Some(sigil_application::ApplicationCommandOutcome::Recovery(
                sigil_application::ApplicationRecoveryOutcome::Fork { session_ref, .. }
            )) = receipt.outcome.as_deref() {
                assert_eq!(session_ref.as_str(), expected_ref);
            }
            let child_records = destination.read_event_records_coordinated()?;
            assert_eq!(child_records.iter().filter(|record| record.stored_event().event_kind()
                == Some(sigil_kernel::DurableEventType::ConversationForked)).count(), 1);
            let stable = std::fs::read(&destination_path)?;
            let second = post_fork(address, &source.id, "same-fork-request", &command).await?;
            assert_eq!(second, replay);
            assert_eq!(std::fs::read(&destination_path)?, stable);
            let source_records = owner.owner.read_handle().read_event_records()?;
            assert_eq!(source_records.iter().filter(|record| matches!(record.session_log_entry(),
                Ok(Some(SessionLogEntry::Control(ControlEntry::ConversationForkCommittedV1(_)))))).count(), 1);
            let entries = driver.session_lifecycle().context("lifecycle")?.catalog()?.entries;
            assert_eq!(entries.len(), 2, "same K/F must not create a second branch");
            let rejected = post_fork(address, &source.id, "new-invalid-route", &different_payload).await?;
            assert!(matches!(rejected, ApplicationCommandReceipt::Rejected(_)),
                "validation before destination publication must remain a definite rejection: {rejected:?}");
            assert_eq!(driver.session_lifecycle().context("lifecycle")?.catalog()?.entries.len(), 2);

            assert!(registry.get_session(&source.id)?.run_ids.is_empty(), "fork must not start a run");
            Ok::<_, anyhow::Error>(())
        }).catch_unwind().await;
        let _ = shutdown_tx.send(());
        serving.await??;
        match result {
            Ok(result) => result?,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
    Ok(())
}

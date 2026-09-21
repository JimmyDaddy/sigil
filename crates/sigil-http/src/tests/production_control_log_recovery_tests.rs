//! Recovery and generation admission through the real authenticated loopback listener.

use super::*;
use crate::{HttpLocalServer, HttpServerConfig};
use sigil_application::{
    CommandJournalBinding, ControlLogRecoveryAction, ControlLogRecoveryOutcome,
};
use sigil_runtime::managed_storage_writer::StorageWriterChannelV1;
use std::net::SocketAddr;

const TOKEN: &str = "control-recovery-test-token";
const CLIENT: &str = "control-recovery-http-client";

async fn request(
    address: SocketAddr,
    method: &str,
    path: &str,
    bearer: bool,
    client: bool,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let body = body.map_or_else(String::new, |body| body.to_string());
    let mut headers = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    if bearer {
        headers.push_str(&format!("Authorization: Bearer {TOKEN}\r\n"));
    }
    if client {
        headers.push_str(&format!("x-sigil-application-client-id: {CLIENT}\r\n"));
    }
    headers.push_str("\r\n");
    headers.push_str(&body);
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("HTTP connection");
        stream
            .write_all(headers.as_bytes())
            .await
            .expect("HTTP request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("HTTP response");
        let split = response
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .expect("complete response headers");
        let head = std::str::from_utf8(&response[..split]).expect("response headers");
        let status = head
            .lines()
            .next()
            .expect("status line")
            .split_whitespace()
            .nth(1)
            .expect("status")
            .parse()
            .expect("numeric status");
        let body = serde_json::from_slice(&response[split + 4..]).expect("JSON response");
        (status, body)
    })
    .await
    .expect("bounded real HTTP operation")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_http_control_recovery_preserves_old_bytes_and_fences_legacy_retries() {
    let fixture = tempfile::tempdir().expect("isolated fixture");
    let driver = production_queue_driver(&fixture, "control-recovery");
    let writer = Arc::clone(
        &driver
            .services
            .authority_composition()
            .expect("real authority composition")
            .storage_writer,
    );
    let source = writer
        .managed_named_leaf_path(
            StorageWriterChannelV1::ApplicationControlLog,
            "http-application",
        )
        .expect("canonical source")
        .join("records.jsonl");
    let lease = writer
        .acquire_named(
            StorageWriterChannelV1::ApplicationControlLog,
            "http-application",
        )
        .expect("real old-generation admission");
    writer
        .write_record(&lease, b"{\"interrupted_reservation\":")
        .expect("inject incomplete JSON payload");
    // Physical framing can settle while the application payload is corrupt. The application
    // owner must keep the original bytes and recover without appending a normal reservation.
    writer
        .finalize(lease)
        .expect("settle test writer's physical framing");
    let original = std::fs::read(&source).expect("original damaged bytes");
    let registry = driver
        .build_registry(Arc::new(
            HttpDurableCommandStore::open(
                fixture.path().join("commands-control-recovery.json"),
                16,
            )
            .expect("isolated command store"),
        ))
        .expect("registry");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("a damaged command journal must not prevent opening a real session");
    let server = HttpLocalServer::bind(
        HttpServerConfig::default(),
        Some(TOKEN),
        Arc::clone(&registry),
    )
    .await
    .expect("real listener");
    let address = server.local_addr().expect("listener address");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(async move {
        server
            .serve_until_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
    });
    let recovery_path = format!("/sessions/{}/application/control-log/recovery", session.id);
    let binding_path = format!(
        "/sessions/{}/application/command-journal-binding",
        session.id
    );
    let commands_path = format!("/sessions/{}/application/commands", session.id);
    let preview_action =
        serde_json::to_value(ControlLogRecoveryAction::Preview).expect("preview wire");

    for path in [&recovery_path, &binding_path] {
        let (method, body) = if path == &recovery_path {
            ("POST", Some(preview_action.clone()))
        } else {
            ("GET", None)
        };
        let (status, denied) = request(address, method, path, false, true, body.clone()).await;
        assert_eq!(status, 401, "missing bearer: {denied}");
        let (status, denied) = request(address, method, path, true, false, body).await;
        assert_eq!(status, 400, "missing client ID: {denied}");
        assert_eq!(denied["error"]["code"], "application_client_id_required");
    }

    // The session and authority are already bound. Recovery must not require an ordinary
    // projection/configuration refresh, which this unrelated broken config would reject.
    let config_path = fixture.path().join("sigil.toml");
    let config = std::fs::read(&config_path).expect("fixture configuration");
    std::fs::write(&config_path, b"config_version = [").expect("break auxiliary refresh input");
    let (status, body) = request(address, "GET", &binding_path, true, true, None).await;
    assert_eq!(status, 200, "binding without projection refresh: {body}");
    let initial: CommandJournalBinding = serde_json::from_value(body).expect("initial binding");
    assert_eq!(initial.command_generation, 0);
    let (status, body) = request(
        address,
        "POST",
        &recovery_path,
        true,
        true,
        Some(preview_action),
    )
    .await;
    assert_eq!(status, 200, "preview without ordinary admission: {body}");
    let ControlLogRecoveryOutcome::Preview(preview) =
        serde_json::from_value(body).expect("preview outcome")
    else {
        panic!("recovery preview expected");
    };
    assert_eq!(preview.authority.request.from_generation, 0);
    assert_eq!(preview.authority.request.successor_generation, 1);
    assert_eq!(preview.authority.old_byte_length, original.len() as u64);
    assert!(preview.impact.tail_command_count_unknown);
    assert_eq!(preview.impact.known_command_count, 0);
    assert_eq!(preview.impact.known_unresolved_count, 0);
    assert!(preview.impact.unparsed_tail_bytes > 0);
    assert!(preview.authority.old_file_identity.is_some());
    assert_eq!(
        preview.authority.old_content_digest.to_hex(),
        sigil_kernel::sha256_hex(&original)
    );
    let confirm = serde_json::to_value(ControlLogRecoveryAction::SealAndRotate {
        preview: preview.clone(),
    })
    .expect("exact confirmation");
    let (status, body) = request(
        address,
        "POST",
        &recovery_path,
        true,
        true,
        Some(confirm.clone()),
    )
    .await;
    assert_eq!(status, 200, "confirm: {body}");
    let ControlLogRecoveryOutcome::Activated(activated) =
        serde_json::from_value(body.clone()).expect("activated outcome")
    else {
        panic!("Activated expected");
    };
    assert_eq!(activated.logical_journal_id, initial.logical_journal_id);
    assert_eq!(activated.command_generation, 1);
    assert_eq!(std::fs::read(&source).expect("sealed source"), original);
    let (status, replay) = request(
        address,
        "POST",
        &recovery_path,
        true,
        true,
        Some(confirm.clone()),
    )
    .await;
    assert_eq!(status, 200, "confirmation response-lost replay: {replay}");
    assert_eq!(replay, body);
    let (status, current) = request(address, "GET", &binding_path, true, true, None).await;
    assert_eq!(status, 200, "current binding: {current}");
    assert_eq!(
        serde_json::from_value::<CommandJournalBinding>(current).expect("current binding"),
        activated
    );
    std::fs::write(&config_path, config).expect("restore configuration before new business");

    let successor = source
        .parent()
        .expect("old namespace")
        .parent()
        .expect("owner root")
        .join(preview.authority.successor_namespace_hash.to_hex())
        .join("records.jsonl");
    let header_only = std::fs::read(&successor).expect("atomic successor header");
    let queue = registry
        .conversation_queue(&session.id)
        .expect("queue before retry");
    assert_eq!(queue.total_items, 0);
    let command = ApplicationCommand::Conversation(ConversationCommand::Queue {
        expected_generation: SafeText::new(queue.generation.0.clone()).expect("queue CAS"),
        action: ApplicationQueueAction::Enqueue {
            target: sigil_application::ApplicationQueueTarget::MainThread,
            prompt: SafeText::new("one explicit new-generation intent").expect("prompt"),
            kind: ApplicationQueueItemKind::Chat,
            reasoning_effort: None,
        },
    });
    for old_binding in [None, Some(initial)] {
        let mut body = serde_json::json!({"command_id":"legacy-unknown-key", "command":command});
        if let Some(binding) = old_binding {
            body["command_journal"] = serde_json::to_value(binding).expect("old binding");
        }
        let (status, denied) =
            request(address, "POST", &commands_path, true, true, Some(body)).await;
        assert_eq!(
            status, 503,
            "sealed unknown request must not be rebound: {denied}"
        );
        assert_eq!(
            std::fs::read(&successor).expect("new-generation bytes"),
            header_only,
            "an old unknown K must not even create a new-generation reservation"
        );
        assert_eq!(
            registry
                .conversation_queue(&session.id)
                .expect("unchanged queue")
                .total_items,
            0
        );
    }

    let new_intent = serde_json::json!({
        "command_id":"new-generation-queue-intent", "command_journal":activated, "command":command,
    });
    let (status, receipt) = request(
        address,
        "POST",
        &commands_path,
        true,
        true,
        Some(new_intent.clone()),
    )
    .await;
    assert_eq!(status, 200, "explicit new-generation intent: {receipt}");
    let ApplicationCommandReceipt::Settled(committed) =
        serde_json::from_value(receipt).expect("causal receipt")
    else {
        panic!("queue domain commit must settle the new intent");
    };
    let queue_after = registry
        .conversation_queue(&session.id)
        .expect("committed queue");
    assert_eq!(queue_after.total_items, 1);
    assert_ne!(
        std::fs::read(&successor).expect("new business records"),
        header_only
    );
    let (status, replay) = request(
        address,
        "POST",
        &commands_path,
        true,
        true,
        Some(new_intent),
    )
    .await;
    assert_eq!(status, 200, "same K/F response-lost retry: {replay}");
    assert_eq!(
        serde_json::from_value::<ApplicationCommandReceipt>(replay).expect("replayed receipt"),
        ApplicationCommandReceipt::Replayed(committed)
    );
    assert_eq!(
        registry
            .conversation_queue(&session.id)
            .expect("replayed queue"),
        queue_after,
        "replaying the original K/F cannot create another queue entry"
    );
    let after_business = std::fs::read(&successor).expect("successor after command");
    let (status, replay) =
        request(address, "POST", &recovery_path, true, true, Some(confirm)).await;
    assert_eq!(
        status, 200,
        "Activated recovery replay after business: {replay}"
    );
    assert_eq!(
        std::fs::read(&successor).expect("same successor business bytes"),
        after_business
    );
    assert_eq!(
        std::fs::read(&source).expect("unchanged old bytes"),
        original
    );
    let _ = shutdown_tx.send(());
    serving
        .await
        .expect("listener task")
        .expect("graceful listener shutdown");
}

//! The common application HTTP route must reach the exact live cancellation owner even when
//! auxiliary configuration or the command journal cannot be read.

use super::*;
use crate::{HttpLocalServer, HttpServerConfig};

async fn post_stop(
    address: std::net::SocketAddr,
    session: &str,
    run: &str,
    id: &str,
) -> (u16, serde_json::Value) {
    let body = serde_json::json!({
        "command_id": id,
        "command": ApplicationCommand::Run(sigil_application::RunCommand::Cancel {
            binding: run.to_owned(), reason: None,
        }),
    })
    .to_string();
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("cancellation request should connect to the local HTTP server");
    stream.write_all(format!(
        "POST /sessions/{session}/application/commands HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer stop-test\r\nx-sigil-application-client-id: stop-client\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len(),
    ).as_bytes()).await.expect("cancellation request should be written completely");
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(15), stream.read_to_end(&mut bytes))
        .await
        .expect("bounded cancellation request")
        .expect("cancellation response should be readable to completion");
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("cancellation response should contain an HTTP header separator");
    let status = std::str::from_utf8(&bytes[..split])
        .expect("cancellation response headers should be UTF-8")
        .split_whitespace()
        .nth(1)
        .expect("cancellation response should contain an HTTP status code")
        .parse()
        .expect("cancellation response status should be numeric");
    (
        status,
        serde_json::from_slice(&bytes[split + 4..])
            .expect("cancellation response body should be JSON"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn common_http_stop_reaches_actual_supervisor_without_projection_or_command_journal()
-> Result<()> {
    for damage_journal in [false, true] {
        let fixture = tempfile::tempdir()?;
        let provider_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let provider_address = provider_listener.local_addr()?;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let provider = tokio::spawn(async move {
            let (mut stream, _) = provider_listener
                .accept()
                .await
                .expect("fixture provider should accept the real run request");
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let read = stream
                    .read(&mut buffer)
                    .await
                    .expect("fixture provider should read the real run request");
                assert!(read > 0, "real provider request must arrive");
                bytes.extend_from_slice(&buffer[..read]);
                assert!(bytes.len() <= 1024 * 1024, "bounded provider fixture");
                if let Some(split) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..split])
                        .expect("provider request headers should be UTF-8");
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length").then(|| {
                                value
                                    .trim()
                                    .parse()
                                    .expect("provider request Content-Length should be numeric")
                            })
                        })
                        .unwrap_or(0);
                    if bytes.len() >= split + 4 + length {
                        break;
                    }
                }
            }
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\n").await.expect("fixture provider should open the SSE response");
            entered_tx
                .send(())
                .expect("test should receive the provider stream entry signal");
            // Keep the real provider stream open until the supervised cancellation settles.
            let _ = release_rx.await;
        });
        let driver = production_queue_driver(&fixture, "common-stop");
        let config_path = fixture.path().join("sigil.toml");
        let config = std::fs::read_to_string(&config_path)?
            .replace("http://127.0.0.1:1", &format!("http://{provider_address}"));
        std::fs::write(&config_path, config)?;
        let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
            fixture.path().join("commands-common-stop.json"),
            16,
        )?))?;
        let session = registry.create_session(HttpSessionCreateRequest::default())?;
        let foreign = registry.create_session(HttpSessionCreateRequest::default())?;
        let client = registry.application_client(&session.id, "stop-client")?;
        let run = registry.start_run(
            &session.id,
            HttpRunStartRequest {
                prompt: "Read the pending stream".to_owned(),
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
        tokio::time::timeout(Duration::from_secs(15), entered_rx).await??;
        std::fs::write(&config_path, "config_version = [")?;
        assert!(
            client.refresh().is_err(),
            "the ordinary projection is unavailable"
        );
        let damaged = if damage_journal {
            let writer = &driver
                .services
                .authority_composition()
                .expect("production fixture should have a managed storage authority")
                .storage_writer;
            let path = writer.managed_named_leaf_path(
                sigil_runtime::managed_storage_writer::StorageWriterChannelV1::ApplicationControlLog,
                "http-application",
            )?.join("records.jsonl");
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            file.write_all(b"{\"partial_stop_test\":")?;
            file.sync_all()?;
            Some((path.clone(), std::fs::read(path)?))
        } else {
            None
        };
        let server = HttpLocalServer::bind(
            HttpServerConfig::default(),
            Some("stop-test"),
            Arc::clone(&registry),
        )
        .await?;
        let address = server.local_addr()?;
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(async move {
            server
                .serve_until_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
        });
        let (status, denied) = post_stop(address, &foreign.id, &run.id, "foreign-stop").await;
        assert_ne!(status, 200, "foreign owner must be rejected: {denied}");
        assert!(!registry.get_run(&run.id)?.status.is_terminal());
        let (status, accepted) = post_stop(address, &session.id, &run.id, "exact-stop").await;
        assert_eq!(status, 200, "exact stop must reach its owner: {accepted}");
        let receipt: ApplicationCommandReceipt = serde_json::from_value(accepted)?;
        if damage_journal {
            assert!(matches!(
                receipt,
                ApplicationCommandReceipt::SafetyStopRequestedButUnrecorded(_)
            ));
        } else {
            assert!(matches!(receipt, ApplicationCommandReceipt::Uncertain(_)));
        }
        tokio::time::timeout(Duration::from_secs(15), async {
            while !registry
                .get_run(&run.id)
                .expect("supervised run should remain registered through cancellation")
                .status
                .is_terminal()
            {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        assert_eq!(registry.get_run(&run.id)?.status, HttpRunStatus::Cancelled);
        if let Some((path, bytes)) = damaged {
            assert_eq!(std::fs::read(path)?, bytes);
        }
        let _ = release_tx.send(());
        provider.await?;
        let _ = shutdown_tx.send(());
        serving.await??;
    }
    Ok(())
}

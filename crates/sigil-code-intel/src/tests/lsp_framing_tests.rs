use std::{path::Path, time::Duration};

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader};

use super::*;

#[tokio::test]
async fn read_lsp_message_decodes_content_length_payload() {
    let message = json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}});
    let encoded = encode_lsp_message(&message).expect("message should encode");
    let mut reader = BufReader::new(std::io::Cursor::new(encoded));

    let decoded = read_lsp_message(&mut reader)
        .await
        .expect("message should decode")
        .expect("message should exist");

    assert_eq!(decoded, message);
}

#[tokio::test]
async fn read_lsp_message_rejects_missing_content_length() {
    let mut reader = BufReader::new(std::io::Cursor::new(
        b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{}".to_vec(),
    ));

    let error = read_lsp_message(&mut reader)
        .await
        .expect_err("missing content length should fail");

    assert!(error.to_string().contains("missing Content-Length header"));
}

#[tokio::test]
async fn read_lsp_message_rejects_invalid_json_body() {
    let mut reader = BufReader::new(std::io::Cursor::new(
        b"Content-Length: 7\r\n\r\nnotjson".to_vec(),
    ));

    let error = read_lsp_message(&mut reader)
        .await
        .expect_err("invalid json should fail");

    assert!(error.to_string().contains("body is not valid json"));
}

#[tokio::test]
async fn lsp_client_initialize_uses_fake_server_capabilities() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    let (client_read, client_write) = tokio::io::split(client_io);
    let (server_read, mut server_write) = tokio::io::split(server_io);
    let mut server_reader = BufReader::new(server_read);
    let server = tokio::spawn(async move {
        let initialize = read_lsp_message(&mut server_reader)
            .await
            .expect("initialize should decode")
            .expect("initialize should exist");
        assert_eq!(
            initialize.get("method").and_then(Value::as_str),
            Some("initialize")
        );
        let id = initialize
            .get("id")
            .and_then(Value::as_u64)
            .expect("initialize id should exist");
        let response = encode_lsp_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "capabilities": { "definitionProvider": true } }
        }))
        .expect("response should encode");
        server_write
            .write_all(&response)
            .await
            .expect("response should write");
        let initialized = read_lsp_message(&mut server_reader)
            .await
            .expect("initialized should decode")
            .expect("initialized should exist");
        assert_eq!(
            initialized.get("method").and_then(Value::as_str),
            Some("initialized")
        );
    });
    let mut client = LspClient::new(client_read, client_write);

    let capabilities = client
        .initialize(Path::new("/tmp"), Value::Null, Duration::from_secs(1))
        .await
        .expect("initialize should complete");

    assert!(definition_supported(&capabilities));
    server.await.expect("fake server task should finish");
}

#[tokio::test]
async fn lsp_client_request_times_out_when_server_does_not_reply() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    let (client_read, client_write) = tokio::io::split(client_io);
    let hold_server = tokio::spawn(async move {
        let _server_io = server_io;
        tokio::time::sleep(Duration::from_millis(100)).await;
    });
    let mut client = LspClient::new(client_read, client_write);

    let error = client
        .request(
            "textDocument/definition",
            Value::Null,
            Duration::from_millis(20),
        )
        .await
        .expect_err("request should time out");

    assert!(error.to_string().contains("timed out"));
    hold_server.await.expect("server hold task should finish");
}

#[tokio::test]
async fn lsp_client_wait_for_diagnostics_reads_publish_diagnostics_notification() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    let (client_read, client_write) = tokio::io::split(client_io);
    let (_server_read, mut server_write) = tokio::io::split(server_io);
    let mut client = LspClient::new(client_read, client_write);
    let uri = "file:///tmp/example.rs";
    let payload = encode_lsp_message(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {
            "uri": uri,
            "diagnostics": [{
                "message": "broken",
                "severity": 1
            }]
        }
    }))
    .expect("payload should encode");

    let writer = tokio::spawn(async move {
        server_write
            .write_all(&payload)
            .await
            .expect("payload should write");
    });

    let diagnostics = client
        .wait_for_diagnostics(uri, Duration::from_secs(1))
        .await
        .expect("diagnostics should read");

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["message"], "broken");
    writer.await.expect("writer task should finish");
}

#[tokio::test]
async fn lsp_client_shutdown_sends_shutdown_and_exit() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    let (client_read, client_write) = tokio::io::split(client_io);
    let (server_read, mut server_write) = tokio::io::split(server_io);
    let mut server_reader = BufReader::new(server_read);
    let server = tokio::spawn(async move {
        let shutdown = read_lsp_message(&mut server_reader)
            .await
            .expect("shutdown should decode")
            .expect("shutdown should exist");
        assert_eq!(
            shutdown.get("method").and_then(Value::as_str),
            Some("shutdown")
        );
        let id = shutdown
            .get("id")
            .and_then(Value::as_u64)
            .expect("shutdown id should exist");
        let response = encode_lsp_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": null
        }))
        .expect("response should encode");
        server_write
            .write_all(&response)
            .await
            .expect("response should write");
        let exit = read_lsp_message(&mut server_reader)
            .await
            .expect("exit should decode")
            .expect("exit should exist");
        assert_eq!(exit.get("method").and_then(Value::as_str), Some("exit"));
    });
    let mut client = LspClient::new(client_read, client_write);

    client
        .shutdown(Duration::from_secs(1))
        .await
        .expect("shutdown should complete");

    server.await.expect("fake server task should finish");
}

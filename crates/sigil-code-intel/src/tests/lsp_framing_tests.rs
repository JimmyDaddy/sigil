use std::{
    future::Future,
    path::Path,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWriteExt, BufReader, ReadBuf};

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
async fn read_lsp_message_rejects_body_limits_before_reading_body() {
    for length in [MAX_LSP_BODY_BYTES + 1, usize::MAX] {
        let header = format!("Content-Length: {length}\r\n\r\n");
        let source = std::io::Cursor::new(header.into_bytes()).chain(FailingReader);
        let mut reader = BufReader::new(source);

        let error = read_lsp_message(&mut reader)
            .await
            .expect_err("oversized body should fail before allocation or body reads");

        assert!(matches!(
            error.downcast_ref::<CodeIntelError>(),
            Some(CodeIntelError::Protocol { reason })
                if reason.contains("message body length")
                    && reason.contains(&MAX_LSP_BODY_BYTES.to_string())
        ));
    }
}

#[tokio::test]
async fn read_lsp_message_accepts_exact_body_limit_and_preserves_next_frame() {
    let mut bytes = format!("Content-Length: {MAX_LSP_BODY_BYTES}\r\n\r\n\"").into_bytes();
    bytes.resize(bytes.len() + MAX_LSP_BODY_BYTES - 2, b'x');
    bytes.push(b'"');
    bytes.extend_from_slice(&encode_lsp_message(&json!({"next": true})).expect("next frame"));
    let mut reader = BufReader::new(std::io::Cursor::new(bytes));

    let value = read_lsp_message(&mut reader)
        .await
        .expect("exact body limit should decode")
        .expect("body should exist");
    let body = value.as_str().expect("body should be a json string");
    assert_eq!(body.len(), MAX_LSP_BODY_BYTES - 2);
    assert!(body.bytes().all(|byte| byte == b'x'));
    assert_eq!(
        read_lsp_message(&mut reader).await.expect("next frame"),
        Some(json!({"next": true}))
    );
}

#[tokio::test]
async fn read_lsp_message_enforces_header_limit_including_terminator() {
    for header_length in [MAX_LSP_HEADER_BYTES, MAX_LSP_HEADER_BYTES + 1] {
        let prefix = "Content-Length: 2\r\nX-Padding: ";
        let padding = "a".repeat(header_length - prefix.len() - 4);
        let header = format!("{prefix}{padding}\r\n\r\n");
        assert_eq!(header.len(), header_length);
        let mut reader = BufReader::new(std::io::Cursor::new(format!("{header}{{}}")));
        let result = read_lsp_message(&mut reader).await;

        if header_length == MAX_LSP_HEADER_BYTES {
            assert_eq!(result.expect("exact header limit"), Some(json!({})));
        } else {
            let error = result.expect_err("terminator must count towards header limit");
            assert!(matches!(
                error.downcast_ref::<CodeIntelError>(),
                Some(CodeIntelError::Protocol { .. })
            ));
        }
    }
}

#[tokio::test]
async fn read_lsp_message_rejects_ambiguous_invalid_and_overflowing_lengths() {
    for header in [
        "Content-Length: 2\r\ncontent-length: 2\r\n\r\n".to_owned(),
        "Content-Length: 2\r\nContent-Length: 999999999999999999999\r\n\r\n".to_owned(),
        "Content-Length: 2\r\nmalformed-field\r\n\r\n".to_owned(),
        "Content-Length: \r\n\r\n".to_owned(),
        "Content-Length: +2\r\n\r\n".to_owned(),
        "Content-Length: -2\r\n\r\n".to_owned(),
        format!("Content-Length: {}\r\n\r\n", "9".repeat(128)),
    ] {
        let mut reader = BufReader::new(std::io::Cursor::new(header).chain(FailingReader));
        let error = read_lsp_message(&mut reader)
            .await
            .expect_err("bad length/header must fail without reading a body");
        assert!(matches!(
            error.downcast_ref::<CodeIntelError>(),
            Some(CodeIntelError::Protocol { .. })
        ));
    }
}

#[tokio::test]
async fn read_lsp_message_distinguishes_clean_eof_from_truncated_frames() {
    let mut empty = BufReader::new(std::io::Cursor::new(Vec::<u8>::new()));
    assert!(
        read_lsp_message(&mut empty)
            .await
            .expect("clean eof")
            .is_none()
    );

    for bytes in [
        b"Content-Length: 2\r\n\r".as_slice(),
        b"Content-Length: 5\r\n\r\n{}".as_slice(),
        b"Content-Length: 0\r\n\r\n".as_slice(),
    ] {
        let mut reader = BufReader::new(std::io::Cursor::new(bytes));
        let error = read_lsp_message(&mut reader)
            .await
            .expect_err("truncated frame or empty json must fail");
        assert!(matches!(
            error.downcast_ref::<CodeIntelError>(),
            Some(CodeIntelError::Protocol { .. })
        ));
    }
}

#[tokio::test]
async fn lsp_client_framing_error_prevents_reusing_body_as_another_frame() {
    for first_frame in [
        format!("Content-Length: {}\r\n\r\n", MAX_LSP_BODY_BYTES + 1).into_bytes(),
        b"Content-Length: 1\r\n\r\n{".to_vec(),
    ] {
        let mut bytes = first_frame;
        bytes.extend_from_slice(
            &encode_lsp_message(&json!({"jsonrpc": "2.0", "id": 2, "result": "wrong"}))
                .expect("trailing bytes"),
        );
        let mut client = LspClient::new(std::io::Cursor::new(bytes), Vec::<u8>::new());
        let error = client
            .request("first", Value::Null, Duration::from_secs(1))
            .await
            .expect_err("bad frame must fail");
        assert!(matches!(
            error.downcast_ref::<CodeIntelError>(),
            Some(CodeIntelError::Protocol { .. })
        ));
        assert!(!client.is_usable());
        let written = client.writer.len();
        let unread = client.reader.buffer().to_vec();

        client
            .request("second", Value::Null, Duration::from_secs(1))
            .await
            .expect_err("broken channel must not send another request");
        client
            .notify("notification", Value::Null)
            .await
            .expect_err("broken channel must not send notifications");
        client
            .wait_for_diagnostics("file:///missing.rs", Duration::from_secs(1))
            .await
            .expect_err("broken channel must not read another frame");
        assert_eq!(client.writer.len(), written);
        assert_eq!(client.reader.buffer(), unread);
    }
}

#[tokio::test]
async fn lsp_client_diagnostics_framing_error_invalidates_channel() {
    let bytes = format!("Content-Length: {}\r\n\r\n", MAX_LSP_BODY_BYTES + 1);
    let mut client = LspClient::new(std::io::Cursor::new(bytes), tokio::io::sink());

    client
        .wait_for_diagnostics("file:///missing.rs", Duration::from_secs(1))
        .await
        .expect_err("diagnostics must enforce the same frame budget");
    assert!(!client.is_usable());
}

#[tokio::test]
async fn lsp_client_partial_frame_timeout_invalidates_channel() {
    let (client_io, mut server_io) = tokio::io::duplex(8192);
    server_io
        .write_all(b"Content-Length: 100\r\n\r\n{")
        .await
        .expect("partial frame");
    let (reader, writer) = tokio::io::split(client_io);
    let mut client = LspClient::new(reader, writer);

    let error = client
        .request("partial", Value::Null, Duration::from_millis(10))
        .await
        .expect_err("partial response must time out");
    assert!(matches!(
        error.downcast_ref::<CodeIntelError>(),
        Some(CodeIntelError::Timeout { .. })
    ));
    assert!(!client.is_usable());
}

#[tokio::test]
async fn lsp_client_write_failure_invalidates_channel() {
    let (client_io, server_io) = tokio::io::duplex(64);
    let (reader, writer) = tokio::io::split(client_io);
    let mut client = LspClient::new(reader, writer);
    drop(server_io);

    let error = client
        .notify("first", Value::Null)
        .await
        .expect_err("closed peer must fail the actual write");
    assert!(matches!(
        error.downcast_ref::<std::io::Error>(),
        Some(error) if error.kind() == std::io::ErrorKind::BrokenPipe
    ));
    assert!(!client.is_usable());

    let error = client
        .notify("second", Value::Null)
        .await
        .expect_err("failed writer must reject subsequent operations");
    assert!(matches!(
        error.downcast_ref::<CodeIntelError>(),
        Some(CodeIntelError::Protocol { .. })
    ));
}

#[tokio::test]
async fn lsp_client_write_cancellation_keeps_partial_frame_channel_unusable() {
    let (client_io, mut server_io) = tokio::io::duplex(8);
    let (reader, writer) = tokio::io::split(client_io);
    let mut client = LspClient::new(reader, writer);
    {
        let notification = client.notify("first", Value::Null);
        tokio::pin!(notification);
        std::future::poll_fn(|cx| {
            assert!(notification.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
    assert!(!client.is_usable());

    let mut written_prefix = [0_u8; 8];
    time::timeout(
        Duration::from_secs(1),
        server_io.read_exact(&mut written_prefix),
    )
    .await
    .expect("partial header must already be in the real duplex buffer")
    .expect("partial header must be readable");
    assert_eq!(&written_prefix, b"Content-");

    let error = time::timeout(Duration::from_secs(1), client.notify("second", Value::Null))
        .await
        .expect("poisoned channel must fail without waiting for a write")
        .expect_err("cancelled write must reject another notification");
    assert!(matches!(
        error.downcast_ref::<CodeIntelError>(),
        Some(CodeIntelError::Protocol { .. })
    ));
    drop(client);
    let mut remaining = Vec::new();
    time::timeout(
        Duration::from_secs(1),
        server_io.read_to_end(&mut remaining),
    )
    .await
    .expect("dropping the client must close its duplex endpoint")
    .expect("remaining bytes must be readable");
    assert!(remaining.is_empty(), "no second frame may be written");
}

#[tokio::test]
async fn lsp_client_jsonrpc_error_keeps_framed_channel_usable() {
    let mut bytes = encode_lsp_message(&json!({
        "jsonrpc": "2.0", "id": 1, "error": {"code": -32603, "message": "request rejected"}
    }))
    .expect("error frame");
    bytes.extend_from_slice(
        &encode_lsp_message(&json!({"jsonrpc": "2.0", "id": 2, "result": "ok"}))
            .expect("success frame"),
    );
    let mut client = LspClient::new(std::io::Cursor::new(bytes), tokio::io::sink());

    client
        .request("first", Value::Null, Duration::from_secs(1))
        .await
        .expect_err("server application error");
    assert!(client.is_usable());
    assert_eq!(
        client
            .request("second", Value::Null, Duration::from_secs(1))
            .await
            .expect("valid channel should remain usable"),
        json!("ok")
    );
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

#[tokio::test]
async fn lsp_client_request_returns_server_errors_and_timeouts() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    let (client_read, client_write) = tokio::io::split(client_io);
    let (server_read, mut server_write) = tokio::io::split(server_io);
    let mut server_reader = BufReader::new(server_read);
    let server = tokio::spawn(async move {
        let request = read_lsp_message(&mut server_reader)
            .await
            .expect("request should decode")
            .expect("request should exist");
        let id = request
            .get("id")
            .and_then(Value::as_u64)
            .expect("request id should exist");
        let response = encode_lsp_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32603, "message": "boom" }
        }))
        .expect("response should encode");
        server_write
            .write_all(&response)
            .await
            .expect("response should write");
    });
    let mut client = LspClient::new(client_read, client_write);

    let error = client
        .request(
            "workspace/symbol",
            json!({ "query": "hello" }),
            Duration::from_secs(1),
        )
        .await
        .expect_err("server error should surface");
    assert!(error.to_string().contains("boom"));

    server.await.expect("fake server task should finish");

    let (client_io, _server_io) = tokio::io::duplex(8192);
    let (client_read, client_write) = tokio::io::split(client_io);
    let mut client = LspClient::new(client_read, client_write);
    let timeout_error = client
        .request(
            "workspace/symbol",
            json!({ "query": "hello" }),
            Duration::from_millis(10),
        )
        .await
        .expect_err("timeout should surface");
    assert!(timeout_error.to_string().contains("workspace/symbol"));
}

#[tokio::test]
async fn lsp_client_wait_for_diagnostics_reads_notifications() {
    let uri = "file:///tmp/workspace/src/main.rs";
    let message = encode_lsp_message(&json!({
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
    .expect("message should encode");
    let mut client = LspClient::new(std::io::Cursor::new(message), tokio::io::sink());

    let diagnostics = client
        .wait_for_diagnostics(uri, Duration::from_secs(1))
        .await
        .expect("diagnostics should arrive");

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["message"], "broken");

    let cached = client
        .wait_for_diagnostics(uri, Duration::from_millis(1))
        .await
        .expect("cached diagnostics should return");
    assert_eq!(cached.len(), 1);
}

#[tokio::test]
async fn lsp_client_ignores_malformed_publish_diagnostics_messages() {
    let uri = "file:///tmp/workspace/src/main.rs";
    let mut client = LspClient::new(std::io::Cursor::new(Vec::<u8>::new()), tokio::io::sink());

    for message in [
        json!({"jsonrpc":"2.0","method":"$/progress"}),
        json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics"}),
        json!({
            "jsonrpc":"2.0",
            "method":"textDocument/publishDiagnostics",
            "params": {"diagnostics": []}
        }),
    ] {
        client.handle_server_message(&message);
    }

    client.handle_server_message(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {
            "uri": uri
        }
    }));

    let diagnostics = client
        .wait_for_diagnostics(uri, Duration::from_millis(1))
        .await
        .expect("cached empty diagnostics should return");

    assert!(diagnostics.is_empty());
}

#[tokio::test]
async fn lsp_client_wait_for_diagnostics_handles_eof_error_and_timeout() {
    let mut client = LspClient::new(std::io::Cursor::new(Vec::<u8>::new()), tokio::io::sink());
    let diagnostics = client
        .wait_for_diagnostics("file:///tmp/missing.rs", Duration::from_millis(1))
        .await
        .expect("eof should return empty diagnostics");
    assert!(diagnostics.is_empty());

    let mut client = LspClient::new(FailingReader, tokio::io::sink());
    let error = client
        .wait_for_diagnostics("file:///tmp/missing.rs", Duration::from_millis(1))
        .await
        .expect_err("reader errors should surface");
    assert!(error.to_string().contains("synthetic read failure"));

    let (client_io, _server_io) = tokio::io::duplex(64);
    let (client_read, client_write) = tokio::io::split(client_io);
    let mut client = LspClient::new(client_read, client_write);
    let diagnostics = client
        .wait_for_diagnostics("file:///tmp/missing.rs", Duration::from_millis(1))
        .await
        .expect("timeout should return empty diagnostics");
    assert!(diagnostics.is_empty());
}

#[tokio::test]
async fn read_lsp_message_reports_protocol_errors() {
    let too_large = format!("Content-Length: 1\r\nX:{}\r\n\r\n", "a".repeat(9000));
    let mut reader = BufReader::new(std::io::Cursor::new(too_large.into_bytes()));
    let error = read_lsp_message(&mut reader)
        .await
        .expect_err("large headers should fail");
    assert!(
        error
            .to_string()
            .contains("message header exceeded 8192 bytes")
    );

    let mut reader = BufReader::new(std::io::Cursor::new(b"Header: ok\r\n\r\n{}".to_vec()));
    let error = read_lsp_message(&mut reader)
        .await
        .expect_err("missing content length should fail");
    assert!(error.to_string().contains("missing Content-Length header"));

    let mut reader = BufReader::new(std::io::Cursor::new(
        b"Content-Length: nope\r\n\r\n{}".to_vec(),
    ));
    let error = read_lsp_message(&mut reader)
        .await
        .expect_err("invalid content length should fail");
    assert!(error.to_string().contains("invalid content length"));

    let body = b"{".to_vec();
    let mut encoded = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    encoded.extend_from_slice(&body);
    let mut reader = BufReader::new(std::io::Cursor::new(encoded));
    let error = read_lsp_message(&mut reader)
        .await
        .expect_err("invalid json should fail");
    assert!(error.to_string().contains("body is not valid json"));

    let mut reader = BufReader::new(std::io::Cursor::new(
        b"Content-Length: 2\r\nX-\xff: bad\r\n\r\n{}".to_vec(),
    ));
    let error = read_lsp_message(&mut reader)
        .await
        .expect_err("invalid utf-8 header should fail");
    assert!(error.to_string().contains("header is not utf-8"));
}

#[test]
fn lsp_helpers_cover_capabilities_paths_and_defaults() {
    assert!(document_symbol_supported(
        &json!({ "documentSymbolProvider": {} })
    ));
    assert!(!definition_supported(
        &json!({ "definitionProvider": false })
    ));
    assert!(references_supported(&json!({ "referencesProvider": true })));
    assert!(code_action_supported(&json!({ "codeActionProvider": {} })));
    assert!(code_action_resolve_supported(
        &json!({ "codeActionProvider": { "resolveProvider": true } })
    ));
    assert!(!code_action_resolve_supported(
        &json!({ "codeActionProvider": true })
    ));
    assert!(rename_supported(&json!({ "renameProvider": {} })));
    assert!(workspace_symbol_supported(
        &json!({ "workspaceSymbolProvider": {} })
    ));
    assert!(diagnostics_supported(&json!({ "diagnosticProvider": {} })));
    assert_eq!(response_array(Value::Null), Vec::<Value>::new());
    assert_eq!(response_array(json!("one")), vec![json!("one")]);
    assert_eq!(
        lsp_error_to_reason(anyhow::anyhow!("")),
        "language server request failed"
    );
}

#[test]
fn lsp_uri_helpers_map_only_workspace_paths() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let file = temp.path().join("src").join("lib.rs");
    std::fs::create_dir_all(file.parent().expect("parent should exist")).expect("dir should write");
    std::fs::write(&file, "pub fn hello() {}\n").expect("file should write");
    let outside = tempfile::NamedTempFile::new().expect("outside file should build");

    let (relative, canonical) = lsp_uri_to_workspace_path(temp.path(), &file_uri_from_path(&file))
        .expect("workspace path should map");
    assert_eq!(relative, "src/lib.rs");
    assert_eq!(canonical, file.canonicalize().expect("canonical path"));
    assert!(lsp_uri_to_workspace_path(temp.path(), &file_uri_from_path(outside.path())).is_none());
}

#[tokio::test]
async fn lsp_client_request_timeout_includes_method_name() {
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
async fn lsp_client_request_times_out_when_server_does_not_reply() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    let (client_read, client_write) = tokio::io::split(client_io);
    let (server_read, _server_write) = tokio::io::split(server_io);
    let mut server_reader = BufReader::new(server_read);
    let server = tokio::spawn(async move {
        let request = read_lsp_message(&mut server_reader)
            .await
            .expect("request should decode")
            .expect("request should exist");
        assert_eq!(request.get("method").and_then(Value::as_str), Some("slow"));
        tokio::time::sleep(Duration::from_millis(100)).await;
    });
    let mut client = LspClient::new(client_read, client_write);

    let error = client
        .request("slow", json!({}), Duration::from_millis(10))
        .await
        .expect_err("request should time out");

    assert!(error.to_string().contains("timed out: slow"));
    server.await.expect("fake server task should finish");
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
async fn read_lsp_message_rejects_missing_content_length_without_jsonrpc_content_type() {
    let bytes = b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{}".to_vec();
    let mut reader = BufReader::new(std::io::Cursor::new(bytes));

    let error = read_lsp_message(&mut reader)
        .await
        .expect_err("missing content length should fail");

    assert!(error.to_string().contains("missing Content-Length"));
}

#[tokio::test]
async fn read_lsp_message_rejects_oversized_header() {
    let header = format!("X-Test: {}\r\n\r\n", "a".repeat(8_200));
    let mut reader = BufReader::new(std::io::Cursor::new(header.into_bytes()));

    let error = read_lsp_message(&mut reader)
        .await
        .expect_err("oversized header should fail");

    assert!(error.to_string().contains("header exceeded 8192 bytes"));
}

#[tokio::test]
async fn read_lsp_message_rejects_short_invalid_json_body() {
    let bytes = b"Content-Length: 3\r\n\r\nnot".to_vec();
    let mut reader = BufReader::new(std::io::Cursor::new(bytes));

    let error = read_lsp_message(&mut reader)
        .await
        .expect_err("invalid json should fail");

    assert!(error.to_string().contains("body is not valid json"));
}

struct FailingReader;

impl AsyncRead for FailingReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Err(std::io::Error::other("synthetic read failure")))
    }
}

#[tokio::test]
async fn wait_for_diagnostics_returns_published_values() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    let (client_read, _client_write) = tokio::io::split(client_io);
    let (_server_read, mut server_write) = tokio::io::split(server_io);
    let uri = "file:///tmp/example.rs";
    let server = tokio::spawn(async move {
        let unrelated = encode_lsp_message(&json!({
            "jsonrpc": "2.0",
            "id": 99,
            "result": null
        }))
        .expect("unrelated message should encode");
        server_write
            .write_all(&unrelated)
            .await
            .expect("unrelated message should write");
        let diagnostics = encode_lsp_message(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "diagnostics": [{ "message": "broken", "severity": 1 }]
            }
        }))
        .expect("diagnostics should encode");
        server_write
            .write_all(&diagnostics)
            .await
            .expect("diagnostics should write");
    });
    let mut client = LspClient::new(client_read, tokio::io::sink());

    let diagnostics = client
        .wait_for_diagnostics(uri, Duration::from_secs(1))
        .await
        .expect("diagnostics should decode");

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["message"], "broken");
    server.await.expect("fake server task should finish");
}

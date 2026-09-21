use super::*;
use crate::{
    DesktopControlLogRecoveryAction as Action, DesktopControlLogRecoveryOutcome as Outcome,
    DesktopControlLogRecoveryPreview as Preview,
};

fn preview() -> Preview {
    serde_json::from_value(serde_json::json!({
        "authority": { "request": {
            "logical_journal_id": "a".repeat(64), "operation_id": "operation-one",
            "from_generation": 0, "successor_generation": 1, "header_digest": "b".repeat(64), "owner_context_digest": "f".repeat(64),
        },
        "old_namespace_hash": "a".repeat(64), "successor_namespace_hash": "c".repeat(64),
        "old_byte_length": 721, "old_content_digest": "d".repeat(64), "preview_digest": "e".repeat(64), "old_file_identity": "f".repeat(64), },
        "impact": { "verified_prefix_bytes": 700, "verified_record_count": 3, "verified_prefix_digest": "a".repeat(64), "known_command_count": 0, "affected_scope_count": 0, "affected_scopes": [], "scopes_truncated": false, "known_unresolved_count": 0, "unresolved_commands": [], "commands_truncated": false, "unparsed_tail_bytes": 21, "tail_command_count_unknown": true },
    })).expect("preview")
}

#[test]
fn recovery_wire_preserves_application_binding_and_ignores_unknown_fields() {
    let action = Action::SealAndRotate {
        preview: Box::new(preview()),
    };
    let wire = serde_json::to_value(&action).expect("wire");
    let application: sigil_application::ControlLogRecoveryAction =
        serde_json::from_value(wire.clone()).expect("application contract");
    assert_eq!(
        serde_json::to_value(application).expect("exact return"),
        wire
    );
    let mut private = serde_json::to_value(preview()).expect("preview wire");
    private["namespace_path"] = serde_json::json!("/private/managed/log");
    let parsed: Preview = serde_json::from_value(private).expect("unknown fields are ignored");
    assert!(
        !serde_json::to_value(parsed)
            .expect("normalized preview")
            .as_object()
            .expect("preview object")
            .contains_key("namespace_path")
    );
    let mut unsafe_number = preview();
    unsafe_number.authority.old_byte_length = 9_007_199_254_740_992;
    assert!(unsafe_number.validate().is_err());
    let mut unknown_tail_hidden = preview();
    unknown_tail_hidden.impact.tail_command_count_unknown = false;
    assert!(unknown_tail_hidden.validate().is_err());
    let mut false_count = preview();
    false_count
        .impact
        .affected_scopes
        .push(crate::DesktopControlLogRecoveryScope {
            scope_digest: "a".repeat(64),
            session_id: Some("session".to_owned()),
            workspace_id: None,
        });
    assert!(false_count.validate().is_err());
}

#[tokio::test]
async fn typed_control_log_client_previews_then_confirms_the_identical_binding() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let client = DesktopHttpClient::new(
        Client::new(),
        address,
        Arc::new(DesktopBearerToken::generate().expect("token")),
    );
    let expected_client_id = client.client_id.to_string();
    let expected = preview();
    let server_preview = expected.clone();
    let server = tokio::spawn(async move {
        for stage in 0..2 {
            let (mut stream, _) = listener.accept().await.expect("connection");
            let mut request = Vec::new();
            let mut chunk = [0u8; 2048];
            let header_end = loop {
                let read = stream.read(&mut chunk).await.expect("request");
                assert!(read > 0);
                request.extend_from_slice(&chunk[..read]);
                if let Some(index) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8(request[..header_end].to_vec())
                .expect("headers")
                .to_ascii_lowercase();
            assert!(
                headers.starts_with("post /sessions/session-1/application/control-log/recovery ")
            );
            assert!(headers.contains("\r\nauthorization: bearer "));
            assert_eq!(
                headers
                    .lines()
                    .find_map(|line| line.strip_prefix("x-sigil-application-client-id: ")),
                Some(expected_client_id.as_str()),
                "both recovery phases must retain the host-bound application client"
            );
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .expect("length")
                .trim()
                .parse::<usize>()
                .expect("length number");
            while request.len() < header_end + length {
                let read = stream.read(&mut chunk).await.expect("body");
                assert!(read > 0);
                request.extend_from_slice(&chunk[..read]);
            }
            let body: serde_json::Value =
                serde_json::from_slice(&request[header_end..header_end + length]).expect("JSON");
            let response = if stage == 0 {
                assert_eq!(body, serde_json::json!("Preview"));
                serde_json::json!({"Preview":server_preview})
            } else {
                assert_eq!(body, serde_json::json!({"SealAndRotate":{"preview":server_preview}}));
                serde_json::json!({"Activated":{"logical_journal_id":"a".repeat(64),"command_generation":1}})
            }.to_string();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.expect("response");
        }
    });
    let Outcome::Preview(observed) = client
        .recover_control_log("session-1", &Action::Preview)
        .await
        .expect("preview")
    else {
        panic!("preview response");
    };
    assert_eq!(*observed, expected);
    assert!(matches!(
        client
            .recover_control_log("session-1", &Action::SealAndRotate { preview: observed })
            .await
            .expect("confirm"),
        Outcome::Activated(_)
    ));
    assert_eq!(
        client.safety_command("session-1", None, ()).command_journal,
        Some(crate::DesktopCommandJournalBinding {
            logical_journal_id: "a".repeat(64),
            command_generation: 1,
        })
    );
    server.await.expect("server");
}

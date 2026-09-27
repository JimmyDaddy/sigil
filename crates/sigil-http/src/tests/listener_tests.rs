use super::*;

#[test]
fn run_recovery_conflict_is_a_typed_409_envelope() {
    let response = registry_error_response(HttpRegistryError::SessionRunRecoveryRequired {
        recovery: crate::HttpSessionRouteRecoveryView {
            code: crate::HttpSessionRouteRecoveryCode::SessionAlreadyActive,
            allowed_actions: vec![
                crate::HttpSessionRouteRecoveryAction::RetrySessionAttach,
                crate::HttpSessionRouteRecoveryAction::StartNewSession,
            ],
            recovery_binding: "sha256:attachment-generation".to_owned(),
            retryable: true,
        },
    });
    assert_eq!(response.status, 409);
    let body: serde_json::Value =
        serde_json::from_slice(&response.body).expect("typed JSON error body");
    assert_eq!(body["error"]["code"], "session_already_active");
    assert_eq!(
        body["error"]["route_recovery"]["recovery_binding"],
        "sha256:attachment-generation"
    );
    assert_eq!(
        body["error"]["route_recovery"]["allowed_actions"][0],
        "retry_session_attach"
    );
}

#[tokio::test]
async fn image_ingress_body_budget_does_not_expand_other_routes()
-> Result<(), Box<dyn std::error::Error>> {
    async fn read_case(path: &str, length: usize, send_body: bool) -> bool {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let path = path.to_owned();
        let writer = tokio::spawn(async move {
            let mut stream = TcpStream::connect(address).await.expect("connect");
            let header = format!(
                "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {length}\r\n\r\n"
            );
            stream.write_all(header.as_bytes()).await.expect("header");
            if send_body {
                stream.write_all(&vec![0; length]).await.expect("body");
            }
        });
        let (mut stream, _) = listener.accept().await.expect("accept");
        let result = read_http_request(&mut stream).await;
        writer.await.expect("writer joined");
        result.is_ok()
    }
    assert!(read_case("/image-attachments", HTTP_MAX_BODY_BYTES + 1, true).await);
    assert!(!read_case("/sessions", HTTP_MAX_BODY_BYTES + 1, false).await);
    assert!(
        !read_case(
            "/image-attachments",
            sigil_kernel::MAX_IMAGE_ATTACHMENT_BYTES as usize + 1,
            false
        )
        .await
    );
    Ok(())
}

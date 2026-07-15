use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc, Barrier, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
};

use serde_json::{Value, json};
use sigil_kernel::{
    PublicRunEvent, PublicRunEventKind, ToolApprovalUserDecision, ToolExecutionId,
    ToolProgressEvent,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::oneshot,
};

use super::{
    DEFAULT_HTTP_TOKEN_ENV, HTTP_PROTOCOL_EVENT_SCHEMA_VERSION, HTTP_PROTOCOL_VERSION,
    HTTP_RUN_EVENT_SSE_NAME, HttpApprovalCommandReceipt, HttpApprovalDecision,
    HttpApprovalDecisionRecord, HttpApprovalDecisionRequest, HttpAuthConfig, HttpAuthError,
    HttpAuthValidator, HttpCommandEnvelope, HttpLiveEventBus, HttpLiveEventRecvError,
    HttpLocalServer, HttpPendingApproval, HttpProtocolEvent, HttpProtocolEventBuffer,
    HttpProtocolEventClass, HttpProtocolEventView, HttpProtocolReplayError,
    HttpProtocolVersionError, HttpRegistryError, HttpRunApprovalMode, HttpRunCancelRequest,
    HttpRunDriver, HttpRunDriverApproval, HttpRunDriverCancel, HttpRunDriverError,
    HttpRunDriverStart, HttpRunEventSequencer, HttpRunStartRequest, HttpRunStatus,
    HttpRunTerminalOutcome, HttpServerConfig, HttpServerConfigError, HttpSessionBinding,
    HttpSessionCreateRequest, HttpSessionRunRegistry, HttpSseError, HttpSseEvent,
    http_openapi_document, public_run_event_to_sse,
};

#[test]
fn module_split_facade_exports_protocol_auth_sse_and_dto_contracts() {
    let envelope = HttpCommandEnvelope::new(
        "command-1",
        "client-1",
        "session-1",
        HttpSessionCreateRequest::default(),
    )
    .with_expected_stream_sequence(7)
    .with_correlation_id("event-1");

    envelope
        .ensure_supported()
        .expect("facade protocol envelope should use the supported version");
    assert_eq!(envelope.protocol_version, HTTP_PROTOCOL_VERSION);
    assert_eq!(envelope.expected_stream_sequence, Some(7));
    assert_eq!(envelope.correlation_id.as_deref(), Some("event-1"));

    let auth = HttpAuthValidator::disabled();
    assert!(!auth.token_required());
    auth.validate_authorization_header(None)
        .expect("disabled auth should accept missing headers");

    assert_eq!(HTTP_RUN_EVENT_SSE_NAME, "run_event");
    assert_eq!(HTTP_PROTOCOL_EVENT_SCHEMA_VERSION, 1);
    assert_eq!(
        HttpRunApprovalMode::AllowReadonly.to_string(),
        "allow_readonly"
    );
}

#[test]
fn default_config_is_localhost_and_token_required() {
    let config = HttpServerConfig::default();

    assert_eq!(config.bind_host, IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(config.port, 0);
    assert_eq!(config.bind_addr(), SocketAddr::from(([127, 0, 0, 1], 0)));
    assert!(config.is_loopback_only());
    assert!(config.token_required());
    assert_eq!(config.auth.token_env, DEFAULT_HTTP_TOKEN_ENV);
    config.validate().expect("default config should be safe");
}

#[test]
fn config_serde_shape_is_snake_case_and_stable() {
    let config = HttpServerConfig {
        bind_host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: 8765,
        auth: HttpAuthConfig {
            require_token: true,
            token_env: "SIGIL_TEST_HTTP_TOKEN".to_owned(),
        },
    };

    let encoded = serde_json::to_value(&config).expect("config should serialize");

    assert_eq!(
        encoded,
        json!({
            "bind_host": "127.0.0.1",
            "port": 8765,
            "auth": {
                "require_token": true,
                "token_env": "SIGIL_TEST_HTTP_TOKEN"
            }
        })
    );

    let decoded: HttpServerConfig =
        serde_json::from_value(encoded).expect("config should deserialize");
    assert_eq!(decoded, config);
    decoded
        .validate()
        .expect("round-tripped config should be valid");
}

#[test]
fn missing_optional_fields_load_secure_defaults() {
    let config: HttpServerConfig =
        serde_json::from_value(json!({"port": 9999})).expect("partial config should load");

    assert_eq!(config.bind_host, IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(config.port, 9999);
    assert!(config.token_required());
    assert_eq!(config.auth.token_env, DEFAULT_HTTP_TOKEN_ENV);
    config
        .validate()
        .expect("partial config should preserve safe defaults");
}

#[test]
fn auth_override_does_not_change_bind_default() {
    let config: HttpServerConfig = serde_json::from_value(json!({
        "auth": {
            "require_token": false,
            "token_env": "IGNORED_WHEN_DISABLED"
        }
    }))
    .expect("auth override should load");

    assert_eq!(config.bind_host, IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert!(!config.token_required());
    assert!(config.is_loopback_only());
    config
        .validate()
        .expect("local explicit auth disable should be valid");
}

#[test]
fn config_validation_rejects_missing_token_env_when_token_required() {
    let config = HttpServerConfig {
        auth: HttpAuthConfig {
            require_token: true,
            token_env: "  ".to_owned(),
        },
        ..HttpServerConfig::default()
    };

    assert_eq!(
        config.validate(),
        Err(HttpServerConfigError::MissingTokenEnv)
    );
    assert_eq!(
        HttpServerConfigError::MissingTokenEnv.to_string(),
        "http auth token env must be set when token auth is required"
    );
}

#[test]
fn config_validation_rejects_external_bind_without_token_auth() {
    let config = HttpServerConfig {
        bind_host: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        auth: HttpAuthConfig {
            require_token: false,
            token_env: DEFAULT_HTTP_TOKEN_ENV.to_owned(),
        },
        ..HttpServerConfig::default()
    };

    assert_eq!(
        config.validate(),
        Err(HttpServerConfigError::ExternalBindWithoutToken)
    );
    assert_eq!(
        HttpServerConfigError::ExternalBindWithoutToken.to_string(),
        "http token auth is required for non-loopback bind addresses"
    );
}

#[test]
fn auth_validator_uses_secure_defaults_and_accepts_matching_bearer() {
    let config = HttpAuthConfig::default();
    assert_eq!(
        config.validator_from_token(None),
        Err(HttpAuthError::MissingToken {
            token_env: DEFAULT_HTTP_TOKEN_ENV.to_owned()
        })
    );
    assert_eq!(
        config.validator_from_token(Some("   ")),
        Err(HttpAuthError::MissingToken {
            token_env: DEFAULT_HTTP_TOKEN_ENV.to_owned()
        })
    );

    let validator = config
        .validator_from_token(Some("  secret-token  "))
        .expect("non-empty token should create validator");
    assert!(validator.token_required());
    validator
        .validate_authorization_header(Some("Bearer secret-token"))
        .expect("matching bearer token should pass");
    validator
        .validate_authorization_header(Some("bearer   secret-token"))
        .expect("bearer scheme should be case insensitive");

    let disabled = HttpAuthConfig {
        require_token: false,
        token_env: "IGNORED".to_owned(),
    }
    .validator_from_token(None)
    .expect("disabled auth should not require a token");
    assert!(!disabled.token_required());
    disabled
        .validate_authorization_header(None)
        .expect("disabled auth should accept missing authorization");
}

#[test]
fn auth_validator_rejects_missing_malformed_and_invalid_headers() {
    let validator = HttpAuthConfig::default()
        .validator_from_token(Some("secret-token"))
        .expect("token should create validator");

    assert_eq!(
        validator.validate_authorization_header(None),
        Err(HttpAuthError::MissingAuthorization)
    );
    assert_eq!(
        validator.validate_authorization_header(Some("  ")),
        Err(HttpAuthError::MissingAuthorization)
    );
    assert_eq!(
        validator.validate_authorization_header(Some("Basic secret-token")),
        Err(HttpAuthError::InvalidAuthorizationScheme)
    );
    assert_eq!(
        validator.validate_authorization_header(Some("Bearer")),
        Err(HttpAuthError::InvalidAuthorizationScheme)
    );
    assert_eq!(
        validator.validate_authorization_header(Some("Bearer wrong-token")),
        Err(HttpAuthError::InvalidToken)
    );
    assert_eq!(
        HttpAuthError::InvalidToken.to_string(),
        "http bearer token is invalid"
    );
}

#[tokio::test]
async fn local_server_binds_loopback_and_serves_health_without_auth() {
    let (address, shutdown, _driver) = spawn_test_http_server().await;

    assert!(address.ip().is_loopback());

    let (status, body) = http_raw_request(
        address,
        "GET /health HTTP/1.1\r\nhost: localhost\r\n\r\n".to_owned(),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["status"], "ok");
    let _ = shutdown.send(());
}

#[tokio::test]
async fn local_server_rejects_unauthenticated_session_command() {
    let (address, shutdown, driver) = spawn_test_http_server().await;

    let body = json!({"label": "desktop"}).to_string();
    let request = format!(
        "POST /sessions HTTP/1.1\r\nhost: localhost\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let (status, body) = http_raw_request(address, request).await;

    assert_eq!(status, 401);
    assert_eq!(body["error"]["code"], "unauthorized");
    assert!(driver.starts().is_empty());
    let _ = shutdown.send(());
}

#[tokio::test]
async fn local_server_routes_run_start_command_and_replays_retry() {
    let (address, shutdown, driver) = spawn_test_http_server().await;

    let session_body = json!({"label": "desktop"}).to_string();
    let session_request = http_post("/sessions", Some("secret-token"), &session_body);
    let (status, session) = http_raw_request(address, session_request).await;
    assert_eq!(status, 201);
    assert_eq!(session["id"], "http-session-1");

    let command = HttpCommandEnvelope::new(
        "command-start-1",
        "desktop-client",
        "http-session-1",
        HttpRunStartRequest {
            prompt: "hello from desktop".to_owned(),
            approval_mode: Some(HttpRunApprovalMode::Ask),
        },
    );
    let command_body = serde_json::to_string(&command).expect("command should serialize");
    let start_request = http_post(
        "/sessions/http-session-1/runs",
        Some("secret-token"),
        &command_body,
    );
    let (status, receipt) = http_raw_request(address, start_request.clone()).await;

    assert_eq!(status, 201);
    assert_eq!(receipt["run"]["id"], "http-run-1");
    assert_eq!(receipt["run"]["status"], "running");
    assert_eq!(receipt["replayed"], false);
    assert_eq!(driver.starts().len(), 1);
    assert_eq!(driver.starts()[0].prompt, "hello from desktop");

    let (retry_status, retry_receipt) = http_raw_request(address, start_request).await;

    assert_eq!(retry_status, 201);
    assert_eq!(retry_receipt["run"]["id"], "http-run-1");
    assert_eq!(retry_receipt["replayed"], true);
    assert_eq!(driver.starts().len(), 1);
    let _ = shutdown.send(());
}

#[tokio::test]
async fn local_server_duplicate_wait_does_not_block_async_health_routing() {
    let (address, shutdown, driver, registry) = spawn_test_http_server_with_registry().await;
    let session_body = json!({"label": "desktop"}).to_string();
    let session_request = http_post("/sessions", Some("secret-token"), &session_body);
    let (status, _session) = http_raw_request(address, session_request).await;
    assert_eq!(status, 201);

    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let observer_release = Arc::clone(&release);
    driver.observe_start(Arc::new(move |_start| {
        entered_tx
            .send(())
            .expect("driver entered signal should send");
        let (lock, ready) = &*observer_release;
        let mut released = lock.lock().expect("release lock should not be poisoned");
        while !*released {
            released = ready
                .wait(released)
                .expect("release lock should not be poisoned");
        }
    }));
    let command = HttpCommandEnvelope::new(
        "command-concurrent-http",
        "desktop-client",
        "http-session-1",
        run_start("hello", HttpRunApprovalMode::Ask),
    );
    let body = serde_json::to_string(&command).expect("command should serialize");
    let request = http_post("/sessions/http-session-1/runs", Some("secret-token"), &body);
    let first_request = request.clone();
    let first = tokio::spawn(async move { http_raw_request(address, first_request).await });
    tokio::task::spawn_blocking(move || entered_rx.recv())
        .await
        .expect("entered waiter should join")
        .expect("driver should enter");
    let second = tokio::spawn(async move { http_raw_request(address, request).await });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if registry.activity().command_waiters == 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "duplicate HTTP command did not enter reservation wait"
        );
        tokio::task::yield_now().await;
    }

    let (health_status, health) = http_raw_request(
        address,
        "GET /health HTTP/1.1\r\nhost: localhost\r\n\r\n".to_owned(),
    )
    .await;
    assert_eq!(health_status, 200);
    assert_eq!(health["status"], "ok");
    let (release_lock, release_ready) = &*release;
    *release_lock
        .lock()
        .expect("release lock should not be poisoned") = true;
    release_ready.notify_all();

    let (first_status, first_receipt) = first.await.expect("first request should join");
    let (second_status, second_receipt) = second.await.expect("second request should join");
    assert_eq!(first_status, 201);
    assert_eq!(second_status, 201);
    assert_eq!(first_receipt["replayed"], false);
    assert_eq!(second_receipt["replayed"], true);
    assert_eq!(driver.starts().len(), 1);
    let _ = shutdown.send(());
}

#[tokio::test]
async fn local_server_returns_503_when_command_capacity_is_exhausted() {
    let (address, shutdown, _driver, registry) = spawn_test_http_server_with_registry().await;
    let session = create_session(&registry, HttpSessionCreateRequest::default());
    for index in 0..256 {
        let command = HttpCommandEnvelope::new(
            format!("capacity-{index}"),
            "client-a",
            &session.id,
            run_start(" ", HttpRunApprovalMode::Ask),
        );
        assert_eq!(
            registry.start_run_command(&session.id, command),
            Err(HttpRegistryError::EmptyPrompt)
        );
    }
    let saturated = HttpCommandEnvelope::new(
        "capacity-256",
        "client-a",
        &session.id,
        run_start(" ", HttpRunApprovalMode::Ask),
    );
    let body = serde_json::to_string(&saturated).expect("command should serialize");
    let request = http_post(
        &format!("/sessions/{}/runs", session.id),
        Some("secret-token"),
        &body,
    );

    let (status, error) = http_raw_request(address, request).await;
    assert_eq!(status, 503);
    assert_eq!(error["error"]["code"], "registry_error");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("identity capacity"))
    );
    let _ = shutdown.send(());
}

#[tokio::test]
async fn local_server_routes_approval_command_and_replays_retry() {
    let (address, shutdown, driver, registry) = spawn_test_http_server_with_registry().await;

    let session_body = json!({"label": "desktop"}).to_string();
    let session_request = http_post("/sessions", Some("secret-token"), &session_body);
    let (status, session) = http_raw_request(address, session_request).await;
    assert_eq!(status, 201);
    assert_eq!(session["id"], "http-session-1");

    let command = HttpCommandEnvelope::new(
        "command-start-1",
        "desktop-client",
        "http-session-1",
        HttpRunStartRequest {
            prompt: "approval needed".to_owned(),
            approval_mode: Some(HttpRunApprovalMode::Ask),
        },
    );
    let command_body = serde_json::to_string(&command).expect("command should serialize");
    let start_request = http_post(
        "/sessions/http-session-1/runs",
        Some("secret-token"),
        &command_body,
    );
    let (status, receipt) = http_raw_request(address, start_request).await;
    assert_eq!(status, 201);
    assert_eq!(receipt["run"]["id"], "http-run-1");

    let waiting = registry
        .register_approval_request("http-run-1", pending_approval("call-1", "write_file"))
        .expect("approval should be pending");
    let approval = HttpCommandEnvelope::new(
        "command-approval-1",
        "desktop-client",
        "http-session-1",
        approval_decision("call-1", HttpApprovalDecision::Approve, None),
    )
    .with_expected_stream_sequence(waiting.stream_sequence)
    .with_correlation_id("event-approval-1");
    let approval_body = serde_json::to_string(&approval).expect("approval should serialize");
    let approval_request = http_post(
        "/runs/http-run-1/approvals/call-1",
        Some("secret-token"),
        &approval_body,
    );
    let (status, receipt) = http_raw_request(address, approval_request.clone()).await;
    assert_eq!(status, 200);
    assert_eq!(receipt["command_id"], "command-approval-1");
    assert_eq!(receipt["decision"]["decision"], "approved");
    assert_eq!(receipt["replayed"], false);
    assert_eq!(driver.approvals().len(), 1);

    let (retry_status, retry_receipt) = http_raw_request(address, approval_request).await;
    assert_eq!(retry_status, 200);
    assert_eq!(retry_receipt["replayed"], true);
    assert_eq!(driver.approvals().len(), 1);
    let _ = shutdown.send(());
}

#[tokio::test]
async fn desktop_adapter_smoke_surface_covers_list_cancel_approval_and_events() {
    let (address, shutdown, driver, registry, event_bus) =
        spawn_test_http_server_with_registry_and_events().await;

    let (status, body) =
        http_raw_request(address, http_get("/sessions", Some("secret-token"), None)).await;
    assert_eq!(status, 200);
    assert_eq!(body["sessions"], json!([]));

    let session_body = json!({"label": "desktop-smoke"}).to_string();
    let (status, session) = http_raw_request(
        address,
        http_post("/sessions", Some("secret-token"), &session_body),
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(session["id"], "http-session-1");

    let (status, listed) =
        http_raw_request(address, http_get("/sessions", Some("secret-token"), None)).await;
    assert_eq!(status, 200);
    assert_eq!(listed["sessions"][0]["label"], "desktop-smoke");

    let (status, fetched_session) = http_raw_request(
        address,
        http_get("/sessions/http-session-1", Some("secret-token"), None),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(fetched_session["id"], "http-session-1");

    let start_command = HttpCommandEnvelope::new(
        "command-start-smoke",
        "desktop-client",
        "http-session-1",
        HttpRunStartRequest {
            prompt: "run desktop smoke".to_owned(),
            approval_mode: Some(HttpRunApprovalMode::Ask),
        },
    );
    let start_body = serde_json::to_string(&start_command).expect("start command should serialize");
    let (status, start_receipt) = http_raw_request(
        address,
        http_post(
            "/sessions/http-session-1/runs",
            Some("secret-token"),
            &start_body,
        ),
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(start_receipt["run"]["id"], "http-run-1");

    let waiting = registry
        .register_approval_request("http-run-1", pending_approval("call-1", "write_file"))
        .expect("approval should be pending");
    let approval_command = HttpCommandEnvelope::new(
        "command-approval-smoke",
        "desktop-client",
        "http-session-1",
        approval_decision(
            "call-1",
            HttpApprovalDecision::Deny,
            Some("smoke".to_owned()),
        ),
    )
    .with_expected_stream_sequence(waiting.stream_sequence);
    let approval_body =
        serde_json::to_string(&approval_command).expect("approval command should serialize");
    let (status, approval_receipt) = http_raw_request(
        address,
        http_post(
            "/runs/http-run-1/approvals/call-1",
            Some("secret-token"),
            &approval_body,
        ),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(approval_receipt["decision"]["decision"], "denied");
    assert_eq!(driver.approvals().len(), 1);

    event_bus
        .publish_run_event(PublicRunEvent::new(
            "http-session-1",
            "http-run-1",
            1,
            PublicRunEventKind::RunStarted {
                prompt: "run desktop smoke".to_owned(),
            },
        ))
        .expect("durable start event should publish");
    event_bus
        .publish_run_event(PublicRunEvent::new(
            "http-session-1",
            "http-run-1",
            2,
            PublicRunEventKind::TextDelta {
                text: "live only".to_owned(),
            },
        ))
        .expect("transient event should publish");
    event_bus
        .publish_run_event(PublicRunEvent::new(
            "http-session-1",
            "http-run-1",
            3,
            PublicRunEventKind::RunFinished {
                final_text: "done".to_owned(),
            },
        ))
        .expect("durable finish event should publish");

    let (event_status, event_content_type, event_body) = http_raw_exchange(
        address,
        http_get("/runs/http-run-1/events", Some("secret-token"), None),
    )
    .await;
    assert_eq!(event_status, 200);
    assert_eq!(event_content_type, "text/event-stream");
    assert!(event_body.contains("id: sigil-http-run-v1:http-session-1:http-run-1:1"));
    assert!(event_body.contains("id: sigil-http-run-v1:http-session-1:http-run-1:3"));
    assert!(event_body.contains("\"type\":\"run_started\""));
    assert!(event_body.contains("\"type\":\"run_finished\""));
    assert!(!event_body.contains("\"type\":\"text_delta\""));

    let (event_status, _event_content_type, event_body) = http_raw_exchange(
        address,
        http_get(
            "/runs/http-run-1/events",
            Some("secret-token"),
            Some("sigil-http-run-v1:http-session-1:http-run-1:1"),
        ),
    )
    .await;
    assert_eq!(event_status, 200);
    assert!(!event_body.contains("\"type\":\"run_started\""));
    assert!(event_body.contains("\"type\":\"run_finished\""));

    let (status, run_before_cancel) = http_raw_request(
        address,
        http_get("/runs/http-run-1", Some("secret-token"), None),
    )
    .await;
    assert_eq!(status, 200);
    let cancel_command = HttpCommandEnvelope::new(
        "command-cancel-smoke",
        "desktop-client",
        "http-session-1",
        HttpRunCancelRequest {
            reason: Some("smoke complete".to_owned()),
        },
    )
    .with_expected_stream_sequence(
        run_before_cancel["stream_sequence"]
            .as_u64()
            .expect("run sequence should be numeric"),
    );
    let cancel_body =
        serde_json::to_string(&cancel_command).expect("cancel command should serialize");
    let cancel_request = http_post(
        "/runs/http-run-1/cancel",
        Some("secret-token"),
        &cancel_body,
    );
    let (status, cancel_receipt) = http_raw_request(address, cancel_request.clone()).await;
    assert_eq!(status, 200);
    assert_eq!(cancel_receipt["run"]["status"], "cancel_requested");
    assert_eq!(cancel_receipt["replayed"], false);
    assert_eq!(driver.cancels().len(), 1);

    let (status, replayed_cancel) = http_raw_request(address, cancel_request).await;
    assert_eq!(status, 200);
    assert_eq!(replayed_cancel["replayed"], true);
    assert_eq!(driver.cancels().len(), 1);

    let _ = shutdown.send(());
}

#[test]
fn openapi_document_covers_current_command_surface_and_approval_guards() {
    let document = http_openapi_document();

    assert_eq!(document["openapi"], "3.1.0");
    assert_eq!(
        document["components"]["securitySchemes"]["BearerAuth"]["scheme"],
        "bearer"
    );
    assert!(
        document["paths"]["/health"]["get"]["security"]
            .as_array()
            .expect("health security should be an array")
            .is_empty()
    );
    assert!(document["paths"]["/sessions"]["post"]["responses"]["401"].is_object());
    assert!(document["paths"]["/sessions"]["get"]["responses"]["200"].is_object());
    assert!(document["paths"]["/sessions"]["post"]["responses"]["500"].is_object());
    assert!(document["paths"]["/sessions/{session_id}"]["get"]["responses"]["404"].is_object());
    assert!(
        document["paths"]["/sessions/{session_id}/runs"]["post"]["responses"]["409"].is_object()
    );
    for path in [
        "/sessions/{session_id}/runs",
        "/runs/{run_id}/cancel",
        "/runs/{run_id}/approvals/{call_id}",
    ] {
        assert!(document["paths"][path]["post"]["responses"]["500"].is_object());
        assert!(document["paths"][path]["post"]["responses"]["503"].is_object());
    }
    assert!(document["paths"]["/runs/{run_id}"]["get"]["responses"]["200"].is_object());
    assert!(document["paths"]["/runs/{run_id}/cancel"]["post"]["responses"]["409"].is_object());
    assert!(document["paths"]["/runs/{run_id}/events"]["get"]["responses"]["200"].is_object());
    assert!(
        document["paths"]["/runs/{run_id}/approvals/{call_id}"]["post"]["responses"]["409"]
            .is_object()
    );
    assert_eq!(
        document["components"]["schemas"]["RunStartCommand"]["allOf"][1]["properties"]["payload"]["$ref"],
        "#/components/schemas/RunStartRequest"
    );
    assert_eq!(
        document["components"]["schemas"]["ApprovalDecisionCommand"]["allOf"][1]["properties"]["payload"]
            ["$ref"],
        "#/components/schemas/ApprovalDecisionRequest"
    );
    assert_eq!(
        document["components"]["schemas"]["RunCancelCommand"]["allOf"][1]["properties"]["payload"]
            ["$ref"],
        "#/components/schemas/RunCancelRequest"
    );
    assert!(
        document["components"]["schemas"]["SessionSnapshot"]["required"]
            .as_array()
            .expect("session required fields")
            .iter()
            .all(|field| field != "created_at_ms")
    );
    let session_required = document["components"]["schemas"]["SessionSnapshot"]["required"]
        .as_array()
        .expect("session required fields");
    for field in ["durable_session_scope_id", "session_log_path"] {
        assert!(session_required.iter().any(|value| value == field));
    }
    let run_statuses = document["components"]["schemas"]["RunStatus"]["enum"]
        .as_array()
        .expect("run status enum");
    for status in ["execution_uncertain", "cancelled", "interrupted"] {
        assert!(run_statuses.iter().any(|value| value == status));
    }
    assert!(
        document["components"]["schemas"]["RunSnapshot"]["required"]
            .as_array()
            .expect("run required fields")
            .contains(&json!("prompt_preview"))
    );
    let command_required = document["components"]["schemas"]["CommandEnvelopeBase"]["required"]
        .as_array()
        .expect("command envelope required fields");
    for field in [
        "protocol_version",
        "command_id",
        "client_id",
        "session_id",
        "payload",
    ] {
        assert!(
            command_required.iter().any(|value| value == field),
            "missing command envelope field {field}"
        );
    }
    let approval_required =
        document["components"]["schemas"]["ApprovalDecisionRequest"]["required"]
            .as_array()
            .expect("approval required fields");
    for field in [
        "approval_request_id",
        "tool_call_hash",
        "policy_version",
        "expires_at_ms",
        "decision",
    ] {
        assert!(
            approval_required.iter().any(|value| value == field),
            "missing approval guard field {field}"
        );
    }
}

#[test]
fn public_run_event_serializes_to_run_event_sse_frame() {
    let event = PublicRunEvent::new(
        "session-1",
        "run-1",
        12,
        PublicRunEventKind::TextDelta {
            text: "hello".to_owned(),
        },
    );

    let sse = public_run_event_to_sse(&event).expect("public run event should serialize");
    let data: Value = serde_json::from_str(sse.data()).expect("sse data should be json");

    assert_eq!(sse.event(), HTTP_RUN_EVENT_SSE_NAME);
    assert_eq!(sse.id(), None);
    assert_eq!(data["schema_version"], HTTP_PROTOCOL_EVENT_SCHEMA_VERSION);
    assert_eq!(data["event_class"], "transient");
    assert_eq!(data.get("replay_id"), None);
    assert_eq!(data["run_event"]["schema_version"], 1);
    assert_eq!(data["run_event"]["session_id"], "session-1");
    assert_eq!(data["run_event"]["run_id"], "run-1");
    assert_eq!(data["run_event"]["sequence"], 12);
    assert_eq!(data["run_event"]["event"]["type"], "text_delta");
    assert_eq!(data["run_event"]["event"]["text"], "hello");
    assert_eq!(
        sse.encode(),
        format!("event: run_event\ndata: {}\n\n", sse.data())
    );
}

#[test]
fn command_envelope_preserves_version_retry_and_stale_client_fields() {
    let envelope = HttpCommandEnvelope::new(
        "command-1",
        "client-tui",
        "session-1",
        json!({ "prompt": "hello" }),
    )
    .with_expected_stream_sequence(42)
    .with_correlation_id("event-1");
    let value = serde_json::to_value(&envelope).expect("command envelope should serialize");
    let decoded: HttpCommandEnvelope<Value> =
        serde_json::from_value(value).expect("command envelope should deserialize");

    assert_eq!(decoded.protocol_version, HTTP_PROTOCOL_VERSION);
    assert_eq!(decoded.command_id, "command-1");
    assert_eq!(decoded.client_id, "client-tui");
    assert_eq!(decoded.session_id, "session-1");
    assert_eq!(decoded.expected_stream_sequence, Some(42));
    assert_eq!(decoded.correlation_id.as_deref(), Some("event-1"));
    assert_eq!(decoded.payload["prompt"], "hello");
    assert_eq!(decoded.ensure_supported(), Ok(()));

    let mut stale = decoded;
    stale.protocol_version = HTTP_PROTOCOL_VERSION + 1;
    assert_eq!(
        stale.ensure_supported(),
        Err(HttpProtocolVersionError::Unsupported {
            supported: HTTP_PROTOCOL_VERSION,
            received: HTTP_PROTOCOL_VERSION + 1,
        })
    );
}

#[test]
fn protocol_event_view_separates_durable_and_transient_shapes() {
    let durable = HttpProtocolEvent::from_run_event(PublicRunEvent::new(
        "session-1",
        "run-1",
        1,
        PublicRunEventKind::RunStarted {
            prompt: "hello".to_owned(),
        },
    ))
    .expect("durable event should build");
    let transient = HttpProtocolEvent::from_run_event(PublicRunEvent::new(
        "session-1",
        "run-1",
        2,
        PublicRunEventKind::ReasoningDelta {
            text: "thinking".to_owned(),
        },
    ))
    .expect("transient event should build");

    match durable.view() {
        HttpProtocolEventView::Durable(view) => {
            assert_eq!(view.schema_version, HTTP_PROTOCOL_EVENT_SCHEMA_VERSION);
            assert_eq!(view.replay_id, "sigil-http-run-v1:session-1:run-1:1");
            assert_eq!(view.run_event.sequence, 1);
        }
        HttpProtocolEventView::Transient(_) => panic!("durable event should use durable view"),
    }
    match transient.view() {
        HttpProtocolEventView::Transient(view) => {
            assert_eq!(view.schema_version, HTTP_PROTOCOL_EVENT_SCHEMA_VERSION);
            assert_eq!(view.run_event.sequence, 2);
        }
        HttpProtocolEventView::Durable(_) => panic!("transient event should use transient view"),
    }
}

#[test]
fn durable_public_run_event_gets_sse_id_and_replay_cursor() {
    let event = PublicRunEvent::new(
        "session-1",
        "run-1",
        2,
        PublicRunEventKind::RunFinished {
            final_text: "done".to_owned(),
        },
    );

    let sse = public_run_event_to_sse(&event).expect("durable event should serialize");
    let data: Value = serde_json::from_str(sse.data()).expect("sse data should be json");

    assert_eq!(sse.id(), Some("sigil-http-run-v1:session-1:run-1:2"));
    assert_eq!(data["replay_id"], "sigil-http-run-v1:session-1:run-1:2");
    assert_eq!(data["event_class"], "durable");
    assert_eq!(data["run_event"]["event"]["type"], "run_finished");
    assert_eq!(
        sse.encode(),
        format!(
            "id: sigil-http-run-v1:session-1:run-1:2\nevent: run_event\ndata: {}\n\n",
            sse.data()
        )
    );
}

#[test]
fn sse_event_encoder_handles_multiline_data_and_rejects_bad_event_names() {
    let event =
        HttpSseEvent::new("debug", "line-1\nline-2").expect("valid event name should create frame");

    assert_eq!(event.event(), "debug");
    assert_eq!(event.data(), "line-1\nline-2");
    assert_eq!(
        event.encode(),
        "event: debug\ndata: line-1\ndata: line-2\n\n"
    );
    assert_eq!(
        HttpSseEvent::new("", "payload"),
        Err(HttpSseError::InvalidEventName {
            event: String::new()
        })
    );
    assert_eq!(
        HttpSseEvent::new("bad\nname", "payload"),
        Err(HttpSseError::InvalidEventName {
            event: "bad\nname".to_owned()
        })
    );
    assert_eq!(
        HttpSseEvent::with_id(Some("bad\nid".to_owned()), "debug", "payload"),
        Err(HttpSseError::InvalidEventId {
            id: "bad\nid".to_owned()
        })
    );
}

#[test]
fn run_event_sequencer_is_monotonic_per_session_run_pair() {
    let sequencer = HttpRunEventSequencer::new();

    let first = sequencer.next_public_event(
        "session-1",
        "run-1",
        PublicRunEventKind::Notice {
            message: "first".to_owned(),
        },
    );
    let second = sequencer.next_public_event(
        "session-1",
        "run-1",
        PublicRunEventKind::RunFinished {
            final_text: "done".to_owned(),
        },
    );
    let other_run = sequencer.next_public_event(
        "session-1",
        "run-2",
        PublicRunEventKind::RunStarted {
            prompt: "hello".to_owned(),
        },
    );
    let other_session =
        sequencer.next_public_event("session-2", "run-1", PublicRunEventKind::RunCancelled);
    let third_sse = sequencer
        .next_sse_event("session-1", "run-1", PublicRunEventKind::RunCancelled)
        .expect("sequenced event should serialize");
    let third_data: Value =
        serde_json::from_str(third_sse.data()).expect("sequenced sse data should be json");

    assert_eq!(first.sequence, 1);
    assert_eq!(second.sequence, 2);
    assert_eq!(other_run.sequence, 1);
    assert_eq!(other_session.sequence, 1);
    assert_eq!(third_data["run_event"]["sequence"], 3);
    assert_eq!(third_sse.id(), Some("sigil-http-run-v1:session-1:run-1:3"));
}

#[test]
fn protocol_event_buffer_replays_only_durable_events_after_last_event_id() {
    let buffer = HttpProtocolEventBuffer::new();
    buffer
        .push_run_event(PublicRunEvent::new(
            "session-1",
            "run-1",
            1,
            PublicRunEventKind::RunStarted {
                prompt: "hello".to_owned(),
            },
        ))
        .expect("durable start event should record");
    let first_delta = buffer
        .push_run_event(PublicRunEvent::new(
            "session-1",
            "run-1",
            2,
            PublicRunEventKind::TextDelta {
                text: "partial".to_owned(),
            },
        ))
        .expect("transient text delta should record");
    let finished = buffer
        .push_run_event(PublicRunEvent::new(
            "session-1",
            "run-1",
            3,
            PublicRunEventKind::RunFinished {
                final_text: "done".to_owned(),
            },
        ))
        .expect("durable finish event should record");
    buffer
        .push_run_event(PublicRunEvent::new(
            "session-1",
            "run-2",
            1,
            PublicRunEventKind::RunStarted {
                prompt: "other".to_owned(),
            },
        ))
        .expect("other run should record");

    assert_eq!(first_delta.event_class, HttpProtocolEventClass::Transient);
    assert_eq!(first_delta.replay_id, None);
    assert_eq!(finished.event_class, HttpProtocolEventClass::Durable);

    let replay = buffer
        .replay_run_after(
            "session-1",
            "run-1",
            Some("sigil-http-run-v1:session-1:run-1:1"),
        )
        .expect("durable cursor should replay later durable events");

    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].run_event.sequence, 3);
    let replay_event =
        serde_json::to_value(&replay[0].run_event.event).expect("replayed event should serialize");
    let finished_event =
        serde_json::to_value(&finished.run_event.event).expect("finished event should serialize");
    assert_eq!(replay_event, finished_event);
}

#[test]
fn protocol_event_buffer_replay_fails_closed_on_bad_or_wrong_cursor() {
    let buffer = HttpProtocolEventBuffer::new();
    buffer
        .push_run_event(PublicRunEvent::new(
            "session-1",
            "run-1",
            1,
            PublicRunEventKind::RunStarted {
                prompt: "hello".to_owned(),
            },
        ))
        .expect("event should record");

    assert!(matches!(
        buffer.replay_run_after("session-1", "run-1", Some("not-a-cursor")),
        Err(HttpProtocolReplayError::InvalidCursor { .. })
    ));
    assert_eq!(
        buffer
            .replay_run_after(
                "session-1",
                "run-1",
                Some("sigil-http-run-v1:session-1:run-2:1")
            )
            .expect_err("wrong run cursor should fail closed"),
        HttpProtocolReplayError::CursorScopeMismatch
    );
    assert_eq!(
        buffer
            .replay_run_after(
                "session-1",
                "run-1",
                Some("sigil-http-run-v1:session-1:run-1:2")
            )
            .expect_err("ahead cursor should fail closed"),
        HttpProtocolReplayError::CursorAhead
    );
}

#[tokio::test]
async fn live_event_bus_delivers_transient_events_without_replay_id() {
    let bus = HttpLiveEventBus::new(8);
    let mut subscriber = bus.subscribe();

    let published = bus
        .publish_run_event(PublicRunEvent::new(
            "session-1",
            "run-1",
            1,
            PublicRunEventKind::ReasoningDelta {
                text: "thinking".to_owned(),
            },
        ))
        .expect("transient event should publish");

    assert_eq!(published.event_class, HttpProtocolEventClass::Transient);
    assert_eq!(published.replay_id, None);
    let live = subscriber
        .recv()
        .await
        .expect("subscriber should receive transient event");
    assert_eq!(live.event_class, HttpProtocolEventClass::Transient);
    assert_eq!(live.run_event.sequence, 1);
    assert!(
        bus.replay_run_after("session-1", "run-1", None)
            .expect("replay should work")
            .is_empty()
    );
}

#[tokio::test]
async fn live_event_bus_treats_tool_progress_as_transient() {
    let bus = HttpLiveEventBus::new(8);
    let progress = ToolProgressEvent {
        execution_id: ToolExecutionId::new("execution-1")
            .expect("test tool execution id should be valid"),
        call_id: "call-1".to_owned(),
        tool_name: "terminal_start".to_owned(),
        sequence: 1,
        status: "running".to_owned(),
        message: Some("running workspace check".to_owned()),
        output_preview: Some("Compiling sigil-tui".to_owned()),
        output_log_ref: Some("state/artifacts/tasks/terminal-1/output.log".into()),
        total_bytes: Some(64),
        updated_at_ms: Some(10),
        details: json!({"task_id": "terminal-1"}),
    };

    let published = bus
        .publish_run_event(PublicRunEvent::new(
            "session-1",
            "run-1",
            1,
            PublicRunEventKind::ToolProgress { progress },
        ))
        .expect("tool progress event should publish");

    assert_eq!(published.event_class, HttpProtocolEventClass::Transient);
    assert_eq!(published.replay_id, None);
    assert!(
        bus.replay_run_after("session-1", "run-1", None)
            .expect("replay should work")
            .is_empty()
    );
}

#[tokio::test]
async fn live_event_bus_reports_lag_without_corrupting_durable_replay() {
    let bus = HttpLiveEventBus::new(1);
    let mut subscriber = bus.subscribe();
    bus.publish_run_event(PublicRunEvent::new(
        "session-1",
        "run-1",
        1,
        PublicRunEventKind::RunStarted {
            prompt: "hello".to_owned(),
        },
    ))
    .expect("start event should publish");
    bus.publish_run_event(PublicRunEvent::new(
        "session-1",
        "run-1",
        2,
        PublicRunEventKind::TextDelta {
            text: "partial".to_owned(),
        },
    ))
    .expect("transient event should publish");
    bus.publish_run_event(PublicRunEvent::new(
        "session-1",
        "run-1",
        3,
        PublicRunEventKind::RunFinished {
            final_text: "done".to_owned(),
        },
    ))
    .expect("finish event should publish");

    assert!(matches!(
        subscriber.recv().await,
        Err(HttpLiveEventRecvError::Lagged { dropped: 2 })
    ));
    let remaining = subscriber
        .recv()
        .await
        .expect("latest event should remain available after lag");
    assert_eq!(remaining.run_event.sequence, 3);

    let replay = bus
        .replay_run_after(
            "session-1",
            "run-1",
            Some("sigil-http-run-v1:session-1:run-1:1"),
        )
        .expect("durable replay should ignore live lag");
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].event_class, HttpProtocolEventClass::Durable);
    assert_eq!(replay[0].run_event.sequence, 3);
}

#[test]
fn crate_dependency_boundary_excludes_tui_and_extra_sigil_crates() {
    let manifest_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let manifest =
        std::fs::read_to_string(&manifest_path).expect("sigil-http manifest should be readable");
    let dependencies = dependency_edges(&manifest);
    let sigil_dependencies = dependencies
        .iter()
        .filter(|(_, name)| name.starts_with("sigil-"))
        .cloned()
        .collect::<Vec<_>>();

    assert!(!dependencies.iter().any(|(_, name)| name == "sigil-tui"));
    assert_eq!(
        sigil_dependencies,
        vec![
            ("dependencies".to_owned(), "sigil-kernel".to_owned()),
            ("dependencies".to_owned(), "sigil-runtime".to_owned())
        ]
    );
}

#[test]
fn session_create_get_returns_stable_snapshot() {
    let (registry, _driver) = registry_with_driver();

    let session = create_session(
        &registry,
        HttpSessionCreateRequest {
            label: Some("mobile-client".to_owned()),
        },
    );

    assert_eq!(session.id, "http-session-1");
    assert_eq!(session.label.as_deref(), Some("mobile-client"));
    assert!(session.run_ids.is_empty());
    assert_eq!(session.durable_session_scope_id, "scope-http-session-1");
    assert_eq!(
        session.session_log_path,
        "/tmp/sigil-http-tests/http-session-1.jsonl"
    );
    assert_eq!(session.foreground_run_id, None);
    assert_eq!(
        registry
            .get_session(&session.id)
            .expect("created session should be readable"),
        session
    );
    assert_eq!(
        registry.get_session("missing"),
        Err(HttpRegistryError::SessionNotFound {
            session_id: "missing".to_owned()
        })
    );
}

#[test]
fn session_creation_fails_closed_without_a_valid_durable_binding() {
    let (registry, driver) = registry_with_driver();
    driver.reject_next_binding("session store unavailable");
    assert_eq!(
        registry.create_session(HttpSessionCreateRequest::default()),
        Err(HttpRegistryError::SessionBindingRejected {
            session_id: "http-session-1".to_owned(),
            message: "session store unavailable".to_owned(),
        })
    );
    assert!(registry.list_sessions().is_empty());

    driver.return_next_binding(HttpSessionBinding {
        session_scope_id: "scope-invalid".to_owned(),
        session_log_path: "relative/session.jsonl".to_owned(),
    });
    assert_eq!(
        registry.create_session(HttpSessionCreateRequest::default()),
        Err(HttpRegistryError::InvalidSessionBinding {
            session_id: "http-session-2".to_owned(),
            message: "durable session log path must be absolute".to_owned(),
        })
    );
    assert!(registry.list_sessions().is_empty());
}

#[test]
fn session_foreground_lease_releases_only_after_typed_terminal() {
    let (registry, _driver) = registry_with_driver();
    let session = create_session(&registry, HttpSessionCreateRequest::default());

    for outcome in [
        HttpRunTerminalOutcome::Finished,
        HttpRunTerminalOutcome::Failed,
        HttpRunTerminalOutcome::Cancelled,
        HttpRunTerminalOutcome::Interrupted,
    ] {
        let run = registry
            .start_run(
                &session.id,
                run_start("foreground", HttpRunApprovalMode::Ask),
            )
            .expect("foreground run should start");
        assert_eq!(
            registry.start_run(
                &session.id,
                run_start("competing", HttpRunApprovalMode::Ask),
            ),
            Err(HttpRegistryError::SessionForegroundRunActive {
                session_id: session.id.clone(),
                run_id: run.id.clone(),
            })
        );
        assert_eq!(
            registry
                .get_session(&session.id)
                .expect("session should remain readable")
                .foreground_run_id
                .as_deref(),
            Some(run.id.as_str())
        );

        let terminal = registry
            .record_run_terminal(&run.id, outcome)
            .expect("typed terminal should release the lease");
        assert_eq!(terminal.status, outcome.status());
        assert_eq!(
            registry
                .record_run_terminal(&run.id, outcome)
                .expect("same terminal should be idempotent"),
            terminal
        );
        assert_eq!(
            registry
                .get_session(&session.id)
                .expect("session should remain readable")
                .foreground_run_id,
            None
        );
    }
}

#[test]
fn contradictory_terminal_callback_fails_closed() {
    let (registry, _driver) = registry_with_driver();
    let session = create_session(&registry, HttpSessionCreateRequest::default());
    let run = registry
        .start_run(&session.id, run_start("terminal", HttpRunApprovalMode::Ask))
        .expect("run should start");
    registry
        .record_run_terminal(&run.id, HttpRunTerminalOutcome::Cancelled)
        .expect("first terminal should win");

    assert_eq!(
        registry.record_run_terminal(&run.id, HttpRunTerminalOutcome::Finished),
        Err(HttpRegistryError::RunTerminalConflict {
            run_id: run.id.clone(),
            current: HttpRunStatus::Cancelled,
            requested: HttpRunTerminalOutcome::Finished,
        })
    );
    assert_eq!(
        registry
            .get_run(&run.id)
            .expect("run should remain inspectable")
            .status,
        HttpRunStatus::Cancelled
    );
}

#[test]
fn driver_panics_quarantine_tentative_start_cancel_and_approval_state() {
    let start_driver = Arc::new(RecordingRunDriver::default());
    let start_registry = HttpSessionRunRegistry::new(start_driver.clone());
    let start_session = create_session(&start_registry, HttpSessionCreateRequest::default());
    start_driver.observe_start(Arc::new(|_start| panic!("start driver panic")));
    assert_eq!(
        start_registry.start_run(
            &start_session.id,
            run_start("panic", HttpRunApprovalMode::Ask),
        ),
        Err(HttpRegistryError::DriverPanicked {
            operation: "start",
            run_id: "http-run-1".to_owned(),
        })
    );
    assert_eq!(
        start_registry
            .get_run("http-run-1")
            .expect("uncertain start should remain inspectable")
            .status,
        HttpRunStatus::ExecutionUncertain
    );
    assert_eq!(
        start_registry
            .get_session(&start_session.id)
            .expect("uncertain session should remain quarantined")
            .foreground_run_id
            .as_deref(),
        Some("http-run-1")
    );
    start_registry
        .record_run_terminal("http-run-1", HttpRunTerminalOutcome::Failed)
        .expect("later durable terminal should resolve uncertain startup");
    assert_eq!(
        start_registry
            .get_session(&start_session.id)
            .expect("confirmed terminal should release the lease")
            .foreground_run_id,
        None
    );

    let cancel_driver = Arc::new(RecordingRunDriver::default());
    let cancel_registry = HttpSessionRunRegistry::new(cancel_driver.clone());
    let cancel_session = create_session(&cancel_registry, HttpSessionCreateRequest::default());
    let cancel_run = cancel_registry
        .start_run(
            &cancel_session.id,
            run_start("cancel panic", HttpRunApprovalMode::Ask),
        )
        .expect("run should start");
    cancel_driver.observe_cancel(Arc::new(|_cancel| panic!("cancel driver panic")));
    assert_eq!(
        cancel_registry.cancel_run(&cancel_run.id),
        Err(HttpRegistryError::DriverPanicked {
            operation: "cancel",
            run_id: cancel_run.id.clone(),
        })
    );
    assert_eq!(
        cancel_registry
            .get_run(&cancel_run.id)
            .expect("uncertain cancel should remain inspectable")
            .status,
        HttpRunStatus::ExecutionUncertain
    );
    cancel_registry
        .record_run_terminal(&cancel_run.id, HttpRunTerminalOutcome::Cancelled)
        .expect("later durable cancellation should replace uncertain projection");

    let approval_driver = Arc::new(RecordingRunDriver::default());
    let approval_registry = HttpSessionRunRegistry::new(approval_driver.clone());
    let approval_session = create_session(&approval_registry, HttpSessionCreateRequest::default());
    let approval_run = approval_registry
        .start_run(
            &approval_session.id,
            run_start("approval panic", HttpRunApprovalMode::Ask),
        )
        .expect("run should start");
    approval_registry
        .register_approval_request(&approval_run.id, pending_approval("call-1", "write_file"))
        .expect("approval should be pending");
    approval_driver.observe_approval(Arc::new(|_approval| panic!("approval driver panic")));
    assert_eq!(
        approval_registry.submit_approval_decision(
            &approval_run.id,
            "call-1",
            approval_decision("call-1", HttpApprovalDecision::Approve, None),
        ),
        Err(HttpRegistryError::DriverPanicked {
            operation: "approval",
            run_id: approval_run.id.clone(),
        })
    );
    let approval_state = approval_registry
        .get_run(&approval_run.id)
        .expect("uncertain approval should remain inspectable");
    assert_eq!(approval_state.status, HttpRunStatus::ExecutionUncertain);
    assert!(approval_state.pending_approval_call_ids.is_empty());
}

#[test]
fn concurrent_duplicate_start_waits_and_replays_one_driver_receipt() {
    let driver = Arc::new(RecordingRunDriver::default());
    let registry = Arc::new(HttpSessionRunRegistry::new(driver.clone()));
    let session = create_session(&registry, HttpSessionCreateRequest::default());
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let calls = Arc::new(AtomicUsize::new(0));
    let observer_entered = Arc::clone(&entered);
    let observer_release = Arc::clone(&release);
    let observer_calls = Arc::clone(&calls);
    driver.observe_start(Arc::new(move |_start| {
        observer_calls.fetch_add(1, Ordering::SeqCst);
        observer_entered.wait();
        observer_release.wait();
    }));
    let command = HttpCommandEnvelope::new(
        "command-concurrent-start",
        "client-a",
        &session.id,
        run_start("hello", HttpRunApprovalMode::Ask),
    );
    let first_registry = Arc::clone(&registry);
    let first_session_id = session.id.clone();
    let first_command = command.clone();
    let first = std::thread::spawn(move || {
        first_registry.start_run_command(&first_session_id, first_command)
    });
    entered.wait();

    let conflicting = HttpCommandEnvelope::new(
        "command-concurrent-start",
        "client-a",
        &session.id,
        run_start("different payload", HttpRunApprovalMode::Ask),
    );
    assert_eq!(
        registry.start_run_command(&session.id, conflicting),
        Err(HttpRegistryError::CommandKeyConflict {
            session_id: session.id.clone(),
            client_id: "client-a".to_owned(),
            command_id: "command-concurrent-start".to_owned(),
        })
    );

    let second_registry = Arc::clone(&registry);
    let second_session_id = session.id.clone();
    let second =
        std::thread::spawn(move || second_registry.start_run_command(&second_session_id, command));
    wait_for_registry_activity(&registry, |activity| activity.command_waiters == 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    release.wait();

    let first = first
        .join()
        .expect("first command thread should join")
        .expect("first command should succeed");
    let second = second
        .join()
        .expect("duplicate command thread should join")
        .expect("duplicate command should replay");
    assert!(!first.replayed);
    assert!(second.replayed);
    assert_eq!(first.run.id, second.run.id);
    assert_eq!(driver.starts().len(), 1);
}

#[test]
fn command_key_conflict_is_global_and_does_not_reuse_receipt() {
    let (registry, driver) = registry_with_driver();
    let session = create_session(&registry, HttpSessionCreateRequest::default());
    let command = HttpCommandEnvelope::new(
        "command-global-key",
        "client-a",
        &session.id,
        run_start("first", HttpRunApprovalMode::Ask),
    );
    let receipt = registry
        .start_run_command(&session.id, command)
        .expect("first command should reserve the key");
    let conflicting = HttpCommandEnvelope::new(
        "command-global-key",
        "client-a",
        &session.id,
        HttpRunCancelRequest::default(),
    );

    assert_eq!(
        registry.cancel_run_command(&receipt.run.id, conflicting),
        Err(HttpRegistryError::CommandKeyConflict {
            session_id: session.id,
            client_id: "client-a".to_owned(),
            command_id: "command-global-key".to_owned(),
        })
    );
    assert!(driver.cancels().is_empty());
}

#[test]
fn command_capacity_fails_closed_without_forgetting_completed_identities() {
    let (registry, _driver) = registry_with_driver();
    let session = create_session(&registry, HttpSessionCreateRequest::default());
    for index in 0..256 {
        let command = HttpCommandEnvelope::new(
            format!("bounded-{index}"),
            "client-a",
            &session.id,
            run_start(" ", HttpRunApprovalMode::Ask),
        );
        assert_eq!(
            registry.start_run_command(&session.id, command),
            Err(HttpRegistryError::EmptyPrompt)
        );
    }

    let saturated = HttpCommandEnvelope::new(
        "bounded-256",
        "client-a",
        &session.id,
        run_start(" ", HttpRunApprovalMode::Ask),
    );
    assert_eq!(
        registry.start_run_command(&session.id, saturated),
        Err(HttpRegistryError::CommandRegistrySaturated)
    );
    let replayed = HttpCommandEnvelope::new(
        "bounded-0",
        "client-a",
        &session.id,
        run_start(" ", HttpRunApprovalMode::Ask),
    );
    assert_eq!(
        registry.start_run_command(&session.id, replayed),
        Err(HttpRegistryError::EmptyPrompt),
        "existing keys must replay even after capacity is reached"
    );
    let conflicting = HttpCommandEnvelope::new(
        "bounded-0",
        "client-a",
        &session.id,
        run_start("\t", HttpRunApprovalMode::Ask),
    );
    assert_eq!(
        registry.start_run_command(&session.id, conflicting),
        Err(HttpRegistryError::CommandKeyConflict {
            session_id: session.id,
            client_id: "client-a".to_owned(),
            command_id: "bounded-0".to_owned(),
        })
    );
    assert_eq!(registry.activity().retained_commands, 256);
    assert_eq!(registry.activity().in_flight_commands, 0);
}

#[test]
fn run_start_requires_session_prompt_and_explicit_approval_mode() {
    let (registry, _driver) = registry_with_driver();

    assert_eq!(
        registry.start_run("missing", run_start("hello", HttpRunApprovalMode::Ask)),
        Err(HttpRegistryError::SessionNotFound {
            session_id: "missing".to_owned()
        })
    );

    let session = create_session(&registry, HttpSessionCreateRequest::default());
    assert_eq!(
        registry.start_run(&session.id, run_start("   ", HttpRunApprovalMode::Ask)),
        Err(HttpRegistryError::EmptyPrompt)
    );
    assert_eq!(
        registry.start_run(
            &session.id,
            HttpRunStartRequest {
                prompt: "hello".to_owned(),
                approval_mode: None,
            }
        ),
        Err(HttpRegistryError::MissingApprovalMode)
    );
}

#[test]
fn run_start_registers_run_and_routes_full_prompt_to_driver() {
    let (registry, driver) = registry_with_driver();
    let session = create_session(
        &registry,
        HttpSessionCreateRequest {
            label: Some("desktop".to_owned()),
        },
    );
    let prompt = format!("{}{}", "x".repeat(120), "tail");

    let run = registry
        .start_run(&session.id, run_start(&prompt, HttpRunApprovalMode::Ask))
        .expect("driver should accept run");

    assert_eq!(run.id, "http-run-1");
    assert_eq!(run.session_id, session.id);
    assert_eq!(run.status, HttpRunStatus::Running);
    assert_eq!(run.approval_mode, HttpRunApprovalMode::Ask);
    assert_eq!(run.prompt_preview, format!("{}...", "x".repeat(120)));
    assert!(run.pending_approval_call_ids.is_empty());
    assert_eq!(
        registry
            .get_session(&session.id)
            .expect("session should be readable")
            .run_ids,
        vec![run.id.clone()]
    );

    let starts = driver.starts();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].session.id, session.id);
    assert_eq!(starts[0].run.status, HttpRunStatus::Starting);
    assert_eq!(starts[0].prompt, prompt);
}

#[test]
fn run_start_driver_failure_marks_run_failed() {
    let (registry, driver) = registry_with_driver();
    driver.reject_next_start("runtime offline");
    let session = create_session(&registry, HttpSessionCreateRequest::default());

    let error = registry
        .start_run(&session.id, run_start("hello", HttpRunApprovalMode::Deny))
        .expect_err("driver failure should reject run start");

    assert_eq!(
        error,
        HttpRegistryError::DriverRejected {
            operation: "start",
            run_id: "http-run-1".to_owned(),
            message: "runtime offline".to_owned(),
        }
    );
    assert_eq!(
        error.to_string(),
        "http driver rejected start for run http-run-1: runtime offline"
    );
    assert_eq!(
        registry
            .get_run("http-run-1")
            .expect("failed run should remain inspectable")
            .status,
        HttpRunStatus::Failed
    );
}

#[test]
fn cancel_routes_to_driver_and_is_idempotent() {
    let (registry, driver) = registry_with_driver();
    let session = create_session(&registry, HttpSessionCreateRequest::default());
    let run = registry
        .start_run(&session.id, run_start("hello", HttpRunApprovalMode::Ask))
        .expect("driver should accept run");

    let canceled = registry
        .cancel_run(&run.id)
        .expect("cancel should route to driver");

    assert_eq!(canceled.status, HttpRunStatus::CancelRequested);
    assert_eq!(
        registry
            .cancel_run(&run.id)
            .expect("repeated cancel should be idempotent")
            .status,
        HttpRunStatus::CancelRequested
    );
    assert_eq!(
        driver.cancels(),
        vec![HttpRunDriverCancel {
            session_id: session.id,
            run_id: run.id,
        }]
    );
    assert_eq!(
        registry.cancel_run("missing"),
        Err(HttpRegistryError::RunNotFound {
            run_id: "missing".to_owned()
        })
    );
}

#[test]
fn concurrent_duplicate_cancel_waits_and_routes_once() {
    let driver = Arc::new(RecordingRunDriver::default());
    let registry = Arc::new(HttpSessionRunRegistry::new(driver.clone()));
    let session = create_session(&registry, HttpSessionCreateRequest::default());
    let run = registry
        .start_run(&session.id, run_start("cancel", HttpRunApprovalMode::Ask))
        .expect("run should start");
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let observer_entered = Arc::clone(&entered);
    let observer_release = Arc::clone(&release);
    driver.observe_cancel(Arc::new(move |_cancel| {
        observer_entered.wait();
        observer_release.wait();
    }));
    let command = HttpCommandEnvelope::new(
        "command-concurrent-cancel",
        "client-a",
        &session.id,
        HttpRunCancelRequest::default(),
    )
    .with_expected_stream_sequence(run.stream_sequence);
    let first_registry = Arc::clone(&registry);
    let first_run_id = run.id.clone();
    let first_command = command.clone();
    let first =
        std::thread::spawn(move || first_registry.cancel_run_command(&first_run_id, first_command));
    entered.wait();

    let second_registry = Arc::clone(&registry);
    let second_run_id = run.id.clone();
    let second =
        std::thread::spawn(move || second_registry.cancel_run_command(&second_run_id, command));
    wait_for_registry_activity(&registry, |activity| activity.command_waiters == 1);
    release.wait();

    let first = first
        .join()
        .expect("first cancel thread should join")
        .expect("first cancel should succeed");
    let second = second
        .join()
        .expect("duplicate cancel thread should join")
        .expect("duplicate cancel should replay");
    assert!(!first.replayed);
    assert!(second.replayed);
    assert_eq!(first.run, second.run);
    assert_eq!(driver.cancels().len(), 1);
}

#[test]
fn distinct_cancel_commands_share_late_driver_rejection() {
    let driver = Arc::new(RecordingRunDriver::default());
    let registry = Arc::new(HttpSessionRunRegistry::new(driver.clone()));
    let session = create_session(&registry, HttpSessionCreateRequest::default());
    let run = registry
        .start_run(
            &session.id,
            run_start("cancel rejection", HttpRunApprovalMode::Ask),
        )
        .expect("run should start");
    driver.reject_next_cancel("cancel route closed");
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let calls = Arc::new(AtomicUsize::new(0));
    let observer_entered = Arc::clone(&entered);
    let observer_release = Arc::clone(&release);
    let observer_calls = Arc::clone(&calls);
    driver.observe_cancel(Arc::new(move |_cancel| {
        observer_calls.fetch_add(1, Ordering::SeqCst);
        observer_entered.wait();
        observer_release.wait();
    }));
    let first_command = HttpCommandEnvelope::new(
        "cancel-first",
        "client-a",
        &session.id,
        HttpRunCancelRequest::default(),
    )
    .with_expected_stream_sequence(run.stream_sequence);
    let first_registry = Arc::clone(&registry);
    let first_run_id = run.id.clone();
    let first =
        std::thread::spawn(move || first_registry.cancel_run_command(&first_run_id, first_command));
    entered.wait();

    let second_command = HttpCommandEnvelope::new(
        "cancel-second",
        "client-b",
        &session.id,
        HttpRunCancelRequest::default(),
    );
    let second_registry = Arc::clone(&registry);
    let second_run_id = run.id.clone();
    let (second_started, second_started_rx) = std::sync::mpsc::channel();
    let second = std::thread::spawn(move || {
        second_started
            .send(())
            .expect("second cancel start signal should send");
        second_registry.cancel_run_command(&second_run_id, second_command)
    });
    second_started_rx
        .recv()
        .expect("second cancel should reach its call boundary");
    wait_for_registry_activity(&registry, |activity| activity.cancellation_waiters == 1);
    release.wait();

    let expected = Err(HttpRegistryError::DriverRejected {
        operation: "cancel",
        run_id: run.id.clone(),
        message: "cancel route closed".to_owned(),
    });
    assert_eq!(
        first.join().expect("first cancel thread should join"),
        expected
    );
    assert_eq!(
        second.join().expect("second cancel thread should join"),
        expected
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        registry
            .get_run(&run.id)
            .expect("rejected cancellation should restore the run")
            .status,
        HttpRunStatus::Running
    );
}

#[test]
fn cancel_rejects_terminal_run_and_restores_status_on_driver_failure() {
    let (registry, driver) = registry_with_driver();
    let session = create_session(&registry, HttpSessionCreateRequest::default());
    driver.reject_next_start("start failed");
    let _error = registry
        .start_run(&session.id, run_start("hello", HttpRunApprovalMode::Ask))
        .expect_err("start should fail");
    assert_eq!(
        registry.cancel_run("http-run-1"),
        Err(HttpRegistryError::RunNotActive {
            run_id: "http-run-1".to_owned()
        })
    );

    let run = registry
        .start_run(&session.id, run_start("second", HttpRunApprovalMode::Ask))
        .expect("second run should start");
    driver.reject_next_cancel("cancel channel closed");

    assert_eq!(
        registry.cancel_run(&run.id),
        Err(HttpRegistryError::DriverRejected {
            operation: "cancel",
            run_id: run.id.clone(),
            message: "cancel channel closed".to_owned(),
        })
    );
    assert_eq!(
        registry
            .get_run(&run.id)
            .expect("run should still be inspectable")
            .status,
        HttpRunStatus::Running
    );
}

#[test]
fn approval_requests_and_decisions_are_routed_in_order() {
    let (registry, driver) = registry_with_driver();
    let session = create_session(&registry, HttpSessionCreateRequest::default());
    let run = registry
        .start_run(
            &session.id,
            run_start("needs tools", HttpRunApprovalMode::Ask),
        )
        .expect("run should start");

    let waiting = registry
        .register_approval_request(&run.id, pending_approval("call-b", "bash"))
        .expect("approval should be registered");
    assert_eq!(waiting.status, HttpRunStatus::WaitingForApproval);
    assert_eq!(waiting.pending_approval_call_ids, vec!["call-b"]);
    let waiting = registry
        .register_approval_request(&run.id, pending_approval("call-a", "read_file"))
        .expect("second approval should be registered");
    assert_eq!(waiting.pending_approval_call_ids, vec!["call-a", "call-b"]);

    let approved = registry
        .submit_approval_decision(
            &run.id,
            "call-a",
            approval_decision(
                "call-a",
                HttpApprovalDecision::Approve,
                Some("read-only".to_owned()),
            ),
        )
        .expect("approval should route");
    assert_eq!(
        approved,
        HttpApprovalDecisionRecord {
            run_id: run.id.clone(),
            call_id: "call-a".to_owned(),
            decision: ToolApprovalUserDecision::Approved,
            reason: Some("read-only".to_owned()),
        }
    );
    assert_eq!(
        registry
            .get_run(&run.id)
            .expect("run should be readable")
            .pending_approval_call_ids,
        vec!["call-b"]
    );

    let denied = registry
        .submit_approval_decision(
            &run.id,
            "call-b",
            approval_decision("call-b", HttpApprovalDecision::Deny, None),
        )
        .expect("denial should route");

    assert_eq!(denied.decision, ToolApprovalUserDecision::Denied);
    assert_eq!(
        registry
            .get_run(&run.id)
            .expect("run should be readable")
            .status,
        HttpRunStatus::Running
    );
    assert_eq!(driver.approvals().len(), 2);
    assert_eq!(driver.approvals()[0].call_id, "call-a");
    assert_eq!(driver.approvals()[1].call_id, "call-b");
}

#[test]
fn approval_command_deduplicates_retries_and_audits_client_fields() {
    let (registry, driver) = registry_with_driver();
    let session = create_session(&registry, HttpSessionCreateRequest::default());
    let run = registry
        .start_run(
            &session.id,
            run_start("needs approval", HttpRunApprovalMode::Ask),
        )
        .expect("run should start");
    let waiting = registry
        .register_approval_request(&run.id, pending_approval("call-1", "write_file"))
        .expect("approval should be pending");
    let command = HttpCommandEnvelope::new(
        "command-approval-1",
        "client-tui",
        &session.id,
        approval_decision("call-1", HttpApprovalDecision::Approve, None),
    )
    .with_expected_stream_sequence(waiting.stream_sequence)
    .with_correlation_id("event-approval-1");

    let receipt = registry
        .submit_approval_command(&run.id, "call-1", command.clone())
        .expect("approval command should route");

    assert_eq!(
        receipt,
        HttpApprovalCommandReceipt {
            command_id: "command-approval-1".to_owned(),
            client_id: "client-tui".to_owned(),
            session_id: session.id.clone(),
            run_id: run.id.clone(),
            call_id: "call-1".to_owned(),
            expected_stream_sequence: Some(waiting.stream_sequence),
            correlation_id: Some("event-approval-1".to_owned()),
            decision: HttpApprovalDecisionRecord {
                run_id: run.id.clone(),
                call_id: "call-1".to_owned(),
                decision: ToolApprovalUserDecision::Approved,
                reason: None,
            },
            replayed: false,
        }
    );

    let replayed = registry
        .submit_approval_command(&run.id, "call-1", command)
        .expect("retried command should replay receipt");

    assert!(replayed.replayed);
    assert_eq!(replayed.command_id, "command-approval-1");
    assert_eq!(driver.approvals().len(), 1);
}

#[test]
fn concurrent_duplicate_approval_waits_and_routes_once() {
    let driver = Arc::new(RecordingRunDriver::default());
    let registry = Arc::new(HttpSessionRunRegistry::new(driver.clone()));
    let session = create_session(&registry, HttpSessionCreateRequest::default());
    let run = registry
        .start_run(&session.id, run_start("approval", HttpRunApprovalMode::Ask))
        .expect("run should start");
    let waiting = registry
        .register_approval_request(&run.id, pending_approval("call-1", "write_file"))
        .expect("approval should be pending");
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let observer_entered = Arc::clone(&entered);
    let observer_release = Arc::clone(&release);
    driver.observe_approval(Arc::new(move |_approval| {
        observer_entered.wait();
        observer_release.wait();
    }));
    let command = HttpCommandEnvelope::new(
        "command-concurrent-approval",
        "client-a",
        &session.id,
        approval_decision("call-1", HttpApprovalDecision::Approve, None),
    )
    .with_expected_stream_sequence(waiting.stream_sequence);
    let first_registry = Arc::clone(&registry);
    let first_run_id = run.id.clone();
    let first_command = command.clone();
    let first = std::thread::spawn(move || {
        first_registry.submit_approval_command(&first_run_id, "call-1", first_command)
    });
    entered.wait();

    let second_registry = Arc::clone(&registry);
    let second_run_id = run.id.clone();
    let second = std::thread::spawn(move || {
        second_registry.submit_approval_command(&second_run_id, "call-1", command)
    });
    wait_for_registry_activity(&registry, |activity| activity.command_waiters == 1);
    release.wait();

    let first = first
        .join()
        .expect("first approval thread should join")
        .expect("first approval should succeed");
    let second = second
        .join()
        .expect("duplicate approval thread should join")
        .expect("duplicate approval should replay");
    assert!(!first.replayed);
    assert!(second.replayed);
    assert_eq!(first.decision, second.decision);
    assert_eq!(driver.approvals().len(), 1);
}

#[test]
fn approval_command_rejects_stale_stream_sequence() {
    let (registry, driver) = registry_with_driver();
    let session = create_session(&registry, HttpSessionCreateRequest::default());
    let run = registry
        .start_run(
            &session.id,
            run_start("needs approval", HttpRunApprovalMode::Ask),
        )
        .expect("run should start");
    registry
        .register_approval_request(&run.id, pending_approval("call-1", "write_file"))
        .expect("approval should be pending");
    let command = HttpCommandEnvelope::new(
        "command-stale",
        "client-tui",
        &session.id,
        approval_decision("call-1", HttpApprovalDecision::Approve, None),
    )
    .with_expected_stream_sequence(0);

    assert_eq!(
        registry.submit_approval_command(&run.id, "call-1", command),
        Err(HttpRegistryError::StaleCommandSequence {
            run_id: run.id.clone(),
            expected: 0,
            actual: 1,
        })
    );
    assert_eq!(
        registry
            .get_run(&run.id)
            .expect("run should be readable")
            .pending_approval_call_ids,
        vec!["call-1"]
    );
    assert!(driver.approvals().is_empty());
}

#[test]
fn approval_command_rejects_changed_tool_call_policy_and_expiry() {
    let (registry, driver) = registry_with_driver();
    let session = create_session(&registry, HttpSessionCreateRequest::default());

    let run = registry
        .start_run(
            &session.id,
            run_start("changed tool call", HttpRunApprovalMode::Ask),
        )
        .expect("run should start");
    registry
        .register_approval_request(&run.id, pending_approval("call-tool", "write_file"))
        .expect("approval should be pending");
    let mut changed_tool = approval_decision(
        "call-tool",
        HttpApprovalDecision::Approve,
        Some("ok".to_owned()),
    );
    changed_tool.tool_call_hash = "hash-changed".to_owned();
    assert_eq!(
        registry.submit_approval_decision(&run.id, "call-tool", changed_tool),
        Err(HttpRegistryError::ApprovalToolCallChanged {
            run_id: run.id.clone(),
            call_id: "call-tool".to_owned(),
        })
    );
    registry
        .record_run_terminal(&run.id, HttpRunTerminalOutcome::Failed)
        .expect("terminal callback should release the foreground lease");

    let run = registry
        .start_run(
            &session.id,
            run_start("changed policy", HttpRunApprovalMode::Ask),
        )
        .expect("run should start");
    registry
        .register_approval_request(&run.id, pending_approval("call-policy", "bash"))
        .expect("approval should be pending");
    let mut changed_policy = approval_decision("call-policy", HttpApprovalDecision::Approve, None);
    changed_policy.policy_version = "policy-v2".to_owned();
    assert_eq!(
        registry.submit_approval_decision(&run.id, "call-policy", changed_policy),
        Err(HttpRegistryError::ApprovalPolicyChanged {
            run_id: run.id.clone(),
            call_id: "call-policy".to_owned(),
        })
    );
    registry
        .record_run_terminal(&run.id, HttpRunTerminalOutcome::Failed)
        .expect("terminal callback should release the foreground lease");

    let run = registry
        .start_run(
            &session.id,
            run_start("changed expiry", HttpRunApprovalMode::Ask),
        )
        .expect("run should start");
    registry
        .register_approval_request(&run.id, pending_approval("call-expiry", "edit_file"))
        .expect("approval should be pending");
    let mut changed_expiry = approval_decision("call-expiry", HttpApprovalDecision::Approve, None);
    changed_expiry.expires_at_ms = u64::MAX - 1;
    assert_eq!(
        registry.submit_approval_decision(&run.id, "call-expiry", changed_expiry),
        Err(HttpRegistryError::ApprovalExpiryChanged {
            run_id: run.id.clone(),
            call_id: "call-expiry".to_owned(),
        })
    );

    assert!(driver.approvals().is_empty());
}

#[test]
fn approval_command_rejects_expired_request_without_consuming_pending_call() {
    let (registry, driver) = registry_with_driver();
    let session = create_session(&registry, HttpSessionCreateRequest::default());
    let run = registry
        .start_run(
            &session.id,
            run_start("expired approval", HttpRunApprovalMode::Ask),
        )
        .expect("run should start");
    let mut expired = pending_approval("call-1", "bash");
    expired.expires_at_ms = 0;
    registry
        .register_approval_request(&run.id, expired)
        .expect("approval should be pending");
    let mut decision = approval_decision("call-1", HttpApprovalDecision::Approve, None);
    decision.expires_at_ms = 0;

    assert_eq!(
        registry.submit_approval_decision(&run.id, "call-1", decision),
        Err(HttpRegistryError::ApprovalExpired {
            run_id: run.id.clone(),
            call_id: "call-1".to_owned(),
        })
    );
    assert_eq!(
        registry
            .get_run(&run.id)
            .expect("run should be readable")
            .pending_approval_call_ids,
        vec!["call-1"]
    );
    assert!(driver.approvals().is_empty());
}

#[test]
fn start_does_not_overwrite_approval_registered_by_driver() {
    let driver = Arc::new(RecordingRunDriver::default());
    let registry = Arc::new(HttpSessionRunRegistry::new(driver.clone()));
    let registry_for_observer = Arc::clone(&registry);
    driver.observe_start(Arc::new(move |start| {
        registry_for_observer
            .register_approval_request(&start.run.id, pending_approval("call-1", "write_file"))
            .expect("driver should be able to register approval during start");
    }));
    let session = create_session(&registry, HttpSessionCreateRequest::default());

    let run = registry
        .start_run(
            &session.id,
            run_start("approval during start", HttpRunApprovalMode::Ask),
        )
        .expect("start should complete");

    assert_eq!(run.status, HttpRunStatus::WaitingForApproval);
    assert_eq!(run.pending_approval_call_ids, vec!["call-1"]);
}

#[test]
fn approval_endpoint_only_accepts_ask_runs() {
    let (registry, _driver) = registry_with_driver();
    let session = create_session(&registry, HttpSessionCreateRequest::default());

    for mode in [
        HttpRunApprovalMode::Deny,
        HttpRunApprovalMode::AllowReadonly,
    ] {
        let run = registry
            .start_run(&session.id, run_start("no approval endpoint", mode))
            .expect("run should start");
        let expected = HttpRegistryError::ApprovalModeDoesNotAsk {
            run_id: run.id.clone(),
            approval_mode: mode,
        };
        assert_eq!(
            registry.register_approval_request(&run.id, pending_approval("call-1", "bash")),
            Err(expected.clone())
        );
        assert_eq!(
            registry.submit_approval_decision(
                &run.id,
                "call-1",
                approval_decision("call-1", HttpApprovalDecision::Approve, None),
            ),
            Err(expected)
        );
        registry
            .record_run_terminal(&run.id, HttpRunTerminalOutcome::Finished)
            .expect("terminal callback should release the foreground lease");
    }
}

#[test]
fn approval_routing_reports_missing_or_inactive_runs() {
    let (registry, _driver) = registry_with_driver();
    assert_eq!(
        registry.register_approval_request("missing", pending_approval("call", "bash")),
        Err(HttpRegistryError::RunNotFound {
            run_id: "missing".to_owned()
        })
    );

    let session = create_session(&registry, HttpSessionCreateRequest::default());
    let run = registry
        .start_run(&session.id, run_start("hello", HttpRunApprovalMode::Ask))
        .expect("run should start");
    assert_eq!(
        registry.submit_approval_decision(
            &run.id,
            "missing-call",
            approval_decision(
                "missing-call",
                HttpApprovalDecision::Deny,
                Some("no".to_owned()),
            ),
        ),
        Err(HttpRegistryError::ApprovalNotPending {
            run_id: run.id.clone(),
            call_id: "missing-call".to_owned(),
        })
    );

    registry
        .cancel_run(&run.id)
        .expect("cancel should mark run cancel requested");
    assert_eq!(
        registry.register_approval_request(&run.id, pending_approval("call", "bash")),
        Err(HttpRegistryError::RunNotActive {
            run_id: run.id.clone(),
        })
    );
    assert_eq!(
        registry.submit_approval_decision(
            &run.id,
            "call",
            approval_decision("call", HttpApprovalDecision::Approve, None),
        ),
        Err(HttpRegistryError::RunNotActive { run_id: run.id })
    );
}

#[test]
fn duplicate_approval_submit_during_driver_route_is_rejected() {
    let driver = Arc::new(RecordingRunDriver::default());
    let registry = Arc::new(HttpSessionRunRegistry::new(driver.clone()));
    let session = create_session(&registry, HttpSessionCreateRequest::default());
    let run = registry
        .start_run(
            &session.id,
            run_start("needs approval", HttpRunApprovalMode::Ask),
        )
        .expect("run should start");
    registry
        .register_approval_request(&run.id, pending_approval("call-1", "write_file"))
        .expect("approval should be pending");
    let duplicate_error = Arc::new(Mutex::new(None::<HttpRegistryError>));
    let duplicate_registry = Arc::clone(&registry);
    let duplicate_run_id = run.id.clone();
    let duplicate_error_slot = Arc::clone(&duplicate_error);
    driver.observe_approval(Arc::new(move |_approval| {
        let result = duplicate_registry.submit_approval_decision(
            &duplicate_run_id,
            "call-1",
            approval_decision(
                "call-1",
                HttpApprovalDecision::Deny,
                Some("duplicate".to_owned()),
            ),
        );
        *lock(&duplicate_error_slot) = Some(result.expect_err("duplicate should be rejected"));
    }));

    let routed = registry
        .submit_approval_decision(
            &run.id,
            "call-1",
            approval_decision("call-1", HttpApprovalDecision::Approve, None),
        )
        .expect("original approval should route");

    assert_eq!(routed.decision, ToolApprovalUserDecision::Approved);
    assert_eq!(
        *lock(&duplicate_error),
        Some(HttpRegistryError::ApprovalNotPending {
            run_id: run.id.clone(),
            call_id: "call-1".to_owned(),
        })
    );
    assert_eq!(driver.approvals().len(), 1);
    assert_eq!(
        registry
            .get_run(&run.id)
            .expect("run should be readable")
            .status,
        HttpRunStatus::Running
    );
}

#[test]
fn approval_driver_failure_keeps_pending_call() {
    let (registry, driver) = registry_with_driver();
    let session = create_session(&registry, HttpSessionCreateRequest::default());
    let run = registry
        .start_run(
            &session.id,
            run_start("needs approval", HttpRunApprovalMode::Ask),
        )
        .expect("run should start");
    registry
        .register_approval_request(&run.id, pending_approval("call-1", "write_file"))
        .expect("approval should be pending");
    driver.reject_next_approval("approval channel closed");

    assert_eq!(
        registry.submit_approval_decision(
            &run.id,
            "call-1",
            approval_decision("call-1", HttpApprovalDecision::Approve, None),
        ),
        Err(HttpRegistryError::DriverRejected {
            operation: "approval",
            run_id: run.id.clone(),
            message: "approval channel closed".to_owned(),
        })
    );
    assert_eq!(
        registry
            .get_run(&run.id)
            .expect("run should be readable")
            .pending_approval_call_ids,
        vec!["call-1"]
    );
}

#[test]
fn run_and_approval_dto_serde_shape_is_snake_case_and_explicit() {
    let start = HttpRunStartRequest {
        prompt: "hello".to_owned(),
        approval_mode: Some(HttpRunApprovalMode::AllowReadonly),
    };
    assert_eq!(
        serde_json::to_value(&start).expect("start request should serialize"),
        json!({
            "prompt": "hello",
            "approval_mode": "allow_readonly"
        })
    );

    let missing_mode: HttpRunStartRequest =
        serde_json::from_value(json!({"prompt": "hello"})).expect("missing mode should parse");
    assert_eq!(missing_mode.approval_mode, None);
    let decision: HttpApprovalDecisionRequest = serde_json::from_value(json!({
        "approval_request_id": "approval-call-1",
        "tool_call_hash": "hash-call-1",
        "policy_version": "policy-v1",
        "expires_at_ms": 9999999999999_u64,
        "decision": "deny"
    }))
    .expect("decision should parse");
    assert_eq!(decision.decision, HttpApprovalDecision::Deny);
    assert_eq!(decision.reason, None);
    assert_eq!(
        HttpRunApprovalMode::AllowReadonly.as_str(),
        "allow_readonly"
    );
    assert!(
        serde_json::from_value::<HttpApprovalDecisionRequest>(json!({})).is_err(),
        "approval decision must be explicit"
    );
}

#[test]
fn run_status_terminal_helper_covers_terminal_and_non_terminal_states() {
    assert!(!HttpRunStatus::Starting.is_terminal());
    assert!(!HttpRunStatus::Running.is_terminal());
    assert!(!HttpRunStatus::WaitingForApproval.is_terminal());
    assert!(!HttpRunStatus::CancelRequested.is_terminal());
    assert!(!HttpRunStatus::ExecutionUncertain.is_terminal());
    assert!(HttpRunStatus::Finished.is_terminal());
    assert!(HttpRunStatus::Failed.is_terminal());
    assert!(HttpRunStatus::Cancelled.is_terminal());
    assert!(HttpRunStatus::Interrupted.is_terminal());
}

fn dependency_edges(manifest: &str) -> Vec<(String, String)> {
    let mut current_section = None::<String>;
    let mut dependencies = Vec::new();

    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            current_section = Some(line.trim_matches(['[', ']']).to_owned());
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(section) = current_section.as_deref() else {
            continue;
        };
        if !section.ends_with("dependencies") {
            continue;
        }
        if let Some((name, _)) = line.split_once('=') {
            dependencies.push((section.to_owned(), name.trim().to_owned()));
        }
    }

    dependencies
}

fn registry_with_driver() -> (HttpSessionRunRegistry, Arc<RecordingRunDriver>) {
    let driver = Arc::new(RecordingRunDriver::default());
    (HttpSessionRunRegistry::new(driver.clone()), driver)
}

fn create_session(
    registry: &HttpSessionRunRegistry,
    request: HttpSessionCreateRequest,
) -> super::HttpSessionSnapshot {
    registry
        .create_session(request)
        .expect("test driver should bind a durable session")
}

fn wait_for_registry_activity(
    registry: &HttpSessionRunRegistry,
    predicate: impl Fn(super::HttpRegistryActivity) -> bool,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let activity = registry.activity();
        if predicate(activity) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "registry activity did not reach expected state: {activity:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn run_start(prompt: &str, approval_mode: HttpRunApprovalMode) -> HttpRunStartRequest {
    HttpRunStartRequest {
        prompt: prompt.to_owned(),
        approval_mode: Some(approval_mode),
    }
}

fn pending_approval(call_id: &str, tool_name: &str) -> HttpPendingApproval {
    HttpPendingApproval {
        call_id: call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        approval_request_id: approval_request_id(call_id),
        tool_call_hash: tool_call_hash(call_id),
        policy_version: policy_version(),
        expires_at_ms: u64::MAX,
    }
}

fn approval_decision(
    call_id: &str,
    decision: HttpApprovalDecision,
    reason: Option<String>,
) -> HttpApprovalDecisionRequest {
    HttpApprovalDecisionRequest {
        approval_request_id: approval_request_id(call_id),
        tool_call_hash: tool_call_hash(call_id),
        policy_version: policy_version(),
        expires_at_ms: u64::MAX,
        decision,
        reason,
    }
}

fn approval_request_id(call_id: &str) -> String {
    format!("approval-{call_id}")
}

fn tool_call_hash(call_id: &str) -> String {
    format!("hash-{call_id}")
}

fn policy_version() -> String {
    "policy-v1".to_owned()
}

#[derive(Default)]
struct RecordingRunDriver {
    starts: Mutex<Vec<HttpRunDriverStart>>,
    cancels: Mutex<Vec<HttpRunDriverCancel>>,
    approvals: Mutex<Vec<HttpRunDriverApproval>>,
    next_start_error: Mutex<Option<String>>,
    next_binding_error: Mutex<Option<String>>,
    next_binding: Mutex<Option<HttpSessionBinding>>,
    next_cancel_error: Mutex<Option<String>>,
    next_approval_error: Mutex<Option<String>>,
    start_observer: Mutex<Option<StartObserver>>,
    cancel_observer: Mutex<Option<CancelObserver>>,
    approval_observer: Mutex<Option<ApprovalObserver>>,
}

impl RecordingRunDriver {
    fn starts(&self) -> Vec<HttpRunDriverStart> {
        lock(&self.starts).clone()
    }

    fn cancels(&self) -> Vec<HttpRunDriverCancel> {
        lock(&self.cancels).clone()
    }

    fn approvals(&self) -> Vec<HttpRunDriverApproval> {
        lock(&self.approvals).clone()
    }

    fn reject_next_start(&self, message: &str) {
        *lock(&self.next_start_error) = Some(message.to_owned());
    }

    fn reject_next_binding(&self, message: &str) {
        *lock(&self.next_binding_error) = Some(message.to_owned());
    }

    fn return_next_binding(&self, binding: HttpSessionBinding) {
        *lock(&self.next_binding) = Some(binding);
    }

    fn reject_next_cancel(&self, message: &str) {
        *lock(&self.next_cancel_error) = Some(message.to_owned());
    }

    fn reject_next_approval(&self, message: &str) {
        *lock(&self.next_approval_error) = Some(message.to_owned());
    }

    fn observe_start(&self, observer: StartObserver) {
        *lock(&self.start_observer) = Some(observer);
    }

    fn observe_approval(&self, observer: ApprovalObserver) {
        *lock(&self.approval_observer) = Some(observer);
    }

    fn observe_cancel(&self, observer: CancelObserver) {
        *lock(&self.cancel_observer) = Some(observer);
    }
}

impl HttpRunDriver for RecordingRunDriver {
    fn bind_session(&self, session_id: &str) -> Result<HttpSessionBinding, HttpRunDriverError> {
        if let Some(message) = lock(&self.next_binding_error).take() {
            return Err(HttpRunDriverError::new(message));
        }
        if let Some(binding) = lock(&self.next_binding).take() {
            return Ok(binding);
        }
        Ok(HttpSessionBinding {
            session_scope_id: format!("scope-{session_id}"),
            session_log_path: format!("/tmp/sigil-http-tests/{session_id}.jsonl"),
        })
    }

    fn start_run(&self, start: HttpRunDriverStart) -> Result<(), HttpRunDriverError> {
        if let Some(message) = lock(&self.next_start_error).take() {
            return Err(HttpRunDriverError::new(message));
        }
        let observer = lock(&self.start_observer).clone();
        if let Some(observer) = observer {
            observer(&start);
        }
        lock(&self.starts).push(start);
        Ok(())
    }

    fn cancel_run(&self, cancel: HttpRunDriverCancel) -> Result<(), HttpRunDriverError> {
        let rejection = lock(&self.next_cancel_error).take();
        let observer = lock(&self.cancel_observer).clone();
        if let Some(observer) = observer {
            observer(&cancel);
        }
        if let Some(message) = rejection {
            return Err(HttpRunDriverError::new(message));
        }
        lock(&self.cancels).push(cancel);
        Ok(())
    }

    fn submit_approval(&self, approval: HttpRunDriverApproval) -> Result<(), HttpRunDriverError> {
        if let Some(message) = lock(&self.next_approval_error).take() {
            return Err(HttpRunDriverError::new(message));
        }
        let observer = lock(&self.approval_observer).clone();
        if let Some(observer) = observer {
            observer(&approval);
        }
        lock(&self.approvals).push(approval);
        Ok(())
    }
}

type StartObserver = Arc<dyn Fn(&HttpRunDriverStart) + Send + Sync>;
type CancelObserver = Arc<dyn Fn(&HttpRunDriverCancel) + Send + Sync>;
type ApprovalObserver = Arc<dyn Fn(&HttpRunDriverApproval) + Send + Sync>;

async fn spawn_test_http_server() -> (SocketAddr, oneshot::Sender<()>, Arc<RecordingRunDriver>) {
    let (address, shutdown, driver, _registry) = spawn_test_http_server_with_registry().await;
    (address, shutdown, driver)
}

async fn spawn_test_http_server_with_registry() -> (
    SocketAddr,
    oneshot::Sender<()>,
    Arc<RecordingRunDriver>,
    Arc<HttpSessionRunRegistry>,
) {
    let (address, shutdown, driver, registry, _event_bus) =
        spawn_test_http_server_with_registry_and_events().await;
    (address, shutdown, driver, registry)
}

async fn spawn_test_http_server_with_registry_and_events() -> (
    SocketAddr,
    oneshot::Sender<()>,
    Arc<RecordingRunDriver>,
    Arc<HttpSessionRunRegistry>,
    Arc<HttpLiveEventBus>,
) {
    let driver = Arc::new(RecordingRunDriver::default());
    let registry = Arc::new(HttpSessionRunRegistry::new(driver.clone()));
    let event_bus = Arc::new(HttpLiveEventBus::new(16));
    let server = HttpLocalServer::bind_with_event_bus(
        HttpServerConfig::default(),
        Some("secret-token"),
        Arc::clone(&registry),
        Arc::clone(&event_bus),
    )
    .await
    .expect("test listener should bind");
    let address = server
        .local_addr()
        .expect("listener address should resolve");
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    tokio::spawn(async move {
        let _ = server
            .serve_until_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });
    (address, shutdown_tx, driver, registry, event_bus)
}

fn http_post(path: &str, token: Option<&str>, body: &str) -> String {
    let auth = token
        .map(|token| format!("authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    format!(
        "POST {path} HTTP/1.1\r\nhost: localhost\r\n{auth}content-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn http_get(path: &str, token: Option<&str>, last_event_id: Option<&str>) -> String {
    let auth = token
        .map(|token| format!("authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let last_event_id = last_event_id
        .map(|id| format!("last-event-id: {id}\r\n"))
        .unwrap_or_default();
    format!("GET {path} HTTP/1.1\r\nhost: localhost\r\n{auth}{last_event_id}\r\n")
}

async fn http_raw_request(address: SocketAddr, request: String) -> (u16, Value) {
    let (status, _content_type, body) = http_raw_exchange(address, request).await;
    let body = serde_json::from_str(&body).expect("response body should be json");
    (status, body)
}

async fn http_raw_exchange(address: SocketAddr, request: String) -> (u16, String, String) {
    let mut stream = TcpStream::connect(address)
        .await
        .expect("test client should connect");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("test client should write request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("test client should read response");
    let text = String::from_utf8(response).expect("response should be utf-8");
    let (head, body) = text
        .split_once("\r\n\r\n")
        .expect("response should have header/body separator");
    let status = head
        .lines()
        .next()
        .expect("response should have status line")
        .split_whitespace()
        .nth(1)
        .expect("response should include status code")
        .parse::<u16>()
        .expect("status code should be numeric");
    let content_type = head
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-type")
                    .then(|| value.trim().to_owned())
            })
        })
        .unwrap_or_default();
    (status, content_type, body.to_owned())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("test lock should not be poisoned")
}

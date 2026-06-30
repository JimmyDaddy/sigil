use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, MutexGuard},
};

use serde_json::{Value, json};
use sigil_kernel::{PublicRunEvent, PublicRunEventKind, ToolApprovalUserDecision};
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
    HttpProtocolVersionError, HttpRegistryError, HttpRunApprovalMode, HttpRunDriver,
    HttpRunDriverApproval, HttpRunDriverCancel, HttpRunDriverError, HttpRunDriverStart,
    HttpRunEventSequencer, HttpRunStartRequest, HttpRunStatus, HttpServerConfig,
    HttpServerConfigError, HttpSessionCreateRequest, HttpSessionRunRegistry, HttpSseError,
    HttpSseEvent, http_openapi_document, public_run_event_to_sse,
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
    assert!(
        document["paths"]["/sessions/{session_id}/runs"]["post"]["responses"]["409"].is_object()
    );
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

    let session = registry.create_session(HttpSessionCreateRequest {
        label: Some("mobile-client".to_owned()),
    });

    assert_eq!(session.id, "http-session-1");
    assert_eq!(session.label.as_deref(), Some("mobile-client"));
    assert!(session.run_ids.is_empty());
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
fn run_start_requires_session_prompt_and_explicit_approval_mode() {
    let (registry, _driver) = registry_with_driver();

    assert_eq!(
        registry.start_run("missing", run_start("hello", HttpRunApprovalMode::Ask)),
        Err(HttpRegistryError::SessionNotFound {
            session_id: "missing".to_owned()
        })
    );

    let session = registry.create_session(HttpSessionCreateRequest::default());
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
    let session = registry.create_session(HttpSessionCreateRequest {
        label: Some("desktop".to_owned()),
    });
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
    let session = registry.create_session(HttpSessionCreateRequest::default());

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
    let session = registry.create_session(HttpSessionCreateRequest::default());
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
fn cancel_rejects_terminal_run_and_restores_status_on_driver_failure() {
    let (registry, driver) = registry_with_driver();
    let session = registry.create_session(HttpSessionCreateRequest::default());
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
    let session = registry.create_session(HttpSessionCreateRequest::default());
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
    let session = registry.create_session(HttpSessionCreateRequest::default());
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
fn approval_command_rejects_stale_stream_sequence() {
    let (registry, driver) = registry_with_driver();
    let session = registry.create_session(HttpSessionCreateRequest::default());
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
    let session = registry.create_session(HttpSessionCreateRequest::default());

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
    let session = registry.create_session(HttpSessionCreateRequest::default());
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
    let session = registry.create_session(HttpSessionCreateRequest::default());

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
    let session = registry.create_session(HttpSessionCreateRequest::default());

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

    let session = registry.create_session(HttpSessionCreateRequest::default());
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
    let session = registry.create_session(HttpSessionCreateRequest::default());
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
    let session = registry.create_session(HttpSessionCreateRequest::default());
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
    assert!(HttpRunStatus::Finished.is_terminal());
    assert!(HttpRunStatus::Failed.is_terminal());
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
    next_cancel_error: Mutex<Option<String>>,
    next_approval_error: Mutex<Option<String>>,
    start_observer: Mutex<Option<StartObserver>>,
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
}

impl HttpRunDriver for RecordingRunDriver {
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
        if let Some(message) = lock(&self.next_cancel_error).take() {
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
    let driver = Arc::new(RecordingRunDriver::default());
    let registry = Arc::new(HttpSessionRunRegistry::new(driver.clone()));
    let server = HttpLocalServer::bind(
        HttpServerConfig::default(),
        Some("secret-token"),
        Arc::clone(&registry),
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
    (address, shutdown_tx, driver, registry)
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

async fn http_raw_request(address: SocketAddr, request: String) -> (u16, Value) {
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
    let body = serde_json::from_str(body).expect("response body should be json");
    (status, body)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("test lock should not be poisoned")
}

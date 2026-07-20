use super::*;

#[test]
fn error_code_projection_accepts_only_bounded_machine_labels() {
    assert_eq!(
        safe_error_code("stale_cursor".to_owned()).as_deref(),
        Some("stale_cursor")
    );
    assert!(safe_error_code("contains space".to_owned()).is_none());
    assert!(safe_error_code("x".repeat(129)).is_none());
}

#[test]
fn typed_client_debug_never_projects_transport_or_bearer_material() {
    let bearer = Arc::new(DesktopBearerToken::generate().expect("token should generate"));
    let client = DesktopHttpClient::new(
        Client::new(),
        "127.0.0.1:3210".parse().expect("address should parse"),
        bearer,
    );
    let debug = format!("{client:?}");

    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("3210"));
}

#[test]
fn run_context_decodes_exact_typed_server_contract() {
    let context: crate::DesktopRunContextView = serde_json::from_value(serde_json::json!({
        "provider_name": "deepseek",
        "model_name": "deepseek-v4-flash",
        "model_selection": "fixed_for_session",
        "available_models": ["deepseek-v4-flash", "deepseek-v4-pro"],
        "default_permission_mode": "manual",
        "available_permission_modes": ["read-only", "manual", "auto-edit", "danger-full-access"],
        "context_window_tokens": 1_000_000,
        "last_prompt_tokens": 42_000,
        "context_window_source": "provider"
    }))
    .expect("run context should decode");

    assert_eq!(context.model_name, "deepseek-v4-flash");
    assert_eq!(context.last_prompt_tokens, Some(42_000));
    assert_eq!(
        context.model_selection,
        crate::DesktopModelSelectionPolicy::FixedForSession
    );
}

#[test]
fn session_management_contract_is_exact_and_path_free() {
    let rename = DesktopSessionRenameRequest {
        session_ref: "managed.jsonl".to_owned(),
        session_id: "durable-managed".to_owned(),
        display_name: "Readable name".to_owned(),
    };
    assert_eq!(
        serde_json::to_value(rename).expect("rename should encode"),
        serde_json::json!({
            "session_ref": "managed.jsonl",
            "session_id": "durable-managed",
            "display_name": "Readable name"
        })
    );
    let receipt = serde_json::from_value::<DesktopSessionMutationReceipt>(serde_json::json!({
        "session_ref": "managed.jsonl",
        "session_id": "durable-managed",
        "operation_id": "session-display-name:1",
        "projection_generation": 2
    }))
    .expect("receipt should decode");
    assert_eq!(receipt.projection_generation, Some(2));
    assert!(!format!("{receipt:?}").contains('/'));

    let quarantine = DesktopSessionQuarantineRequest {
        session_ref: "broken.jsonl".to_owned(),
        source_bytes: 17,
        source_modified_at_unix_ms: 42,
    };
    assert_eq!(
        serde_json::to_value(quarantine).expect("quarantine should encode"),
        serde_json::json!({
            "session_ref": "broken.jsonl",
            "source_bytes": 17,
            "source_modified_at_unix_ms": 42
        })
    );
    let quarantine_receipt =
        serde_json::from_value::<DesktopSessionQuarantineReceipt>(serde_json::json!({
            "session_ref": "broken.jsonl",
            "operation_id": "session-quarantine:1",
            "quarantine_name": "1--broken.jsonl",
            "projection_generation": 3
        }))
        .expect("quarantine receipt should decode");
    assert_eq!(quarantine_receipt.projection_generation, Some(3));
}

#[tokio::test]
async fn transcript_query_rejects_unbounded_renderer_values_before_transport() {
    let bearer = Arc::new(DesktopBearerToken::generate().expect("token should generate"));
    let client = DesktopHttpClient::new(
        Client::new(),
        "127.0.0.1:3210".parse().expect("address should parse"),
        bearer,
    );

    assert!(matches!(
        client
            .transcript(
                "session-1",
                &DesktopTranscriptQuery {
                    before: None,
                    limit: Some(101),
                },
            )
            .await,
        Err(DesktopClientError::InvalidRoute)
    ));
    assert!(matches!(
        client
            .transcript(
                "session-1",
                &DesktopTranscriptQuery {
                    before: Some(0),
                    limit: Some(50),
                },
            )
            .await,
        Err(DesktopClientError::InvalidRoute)
    ));
}

#[test]
fn sse_decoder_accepts_durable_and_transient_frames_and_rejects_gaps() {
    let durable = br#"id: sigil-http-run-v1:session-1:run-1:1
event: run_event
data: {"schema_version":1,"event_class":"durable","replay_id":"sigil-http-run-v1:session-1:run-1:1","run_event":{"schema_version":1,"session_id":"session-1","run_id":"run-1","sequence":1,"event":{"type":"run_started","prompt":"hello"}}}
"#;
    let decoded = decode_sse_frame(durable, "session-1", "run-1")
        .expect("frame should decode")
        .expect("frame should contain an event");
    assert_eq!(decoded.run_event.sequence, 1);

    let transient = br#"event: run_event
data: {"schema_version":1,"event_class":"transient","run_event":{"schema_version":1,"session_id":"session-1","run_id":"run-1","sequence":2,"event":{"type":"text_delta","text":"live"}}}
"#;
    let decoded = decode_sse_frame(transient, "session-1", "run-1")
        .expect("frame should decode")
        .expect("frame should contain an event");
    assert_eq!(decoded.event_class, DesktopProtocolEventClass::Transient);

    let gap = br#"event: stream_gap
data: {"dropped_live_events":1}
"#;
    assert!(matches!(
        decode_sse_frame(gap, "session-1", "run-1"),
        Err(DesktopClientError::EventStreamGap)
    ));
}

#[test]
fn sse_decoder_rejects_cursor_or_stream_mismatch() {
    let mismatched_cursor = br#"id: cursor-other
event: run_event
data: {"schema_version":1,"event_class":"durable","replay_id":"sigil-http-run-v1:session-1:run-1:1","run_event":{"schema_version":1,"session_id":"session-1","run_id":"run-1","sequence":1,"event":{"type":"run_started","prompt":"hello"}}}
"#;
    assert!(matches!(
        decode_sse_frame(mismatched_cursor, "session-1", "run-1"),
        Err(DesktopClientError::InvalidEventStream)
    ));

    let wrong_run = br#"event: run_event
data: {"schema_version":1,"event_class":"transient","run_event":{"schema_version":1,"session_id":"session-1","run_id":"run-other","sequence":2,"event":{"type":"text_delta","text":"live"}}}
"#;
    assert!(matches!(
        decode_sse_frame(wrong_run, "session-1", "run-1"),
        Err(DesktopClientError::ProtocolEvent(
            DesktopProtocolEventError::WrongStream
        ))
    ));
}

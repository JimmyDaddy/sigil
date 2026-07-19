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

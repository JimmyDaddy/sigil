use serde_json::json;

use super::*;

fn envelope(event_class: DesktopProtocolEventClass, event: Value) -> DesktopProtocolEvent {
    DesktopProtocolEvent {
        schema_version: DESKTOP_PROTOCOL_EVENT_SCHEMA_VERSION,
        event_class,
        replay_id: (event_class == DesktopProtocolEventClass::Durable)
            .then(|| "sigil-http-run-v1:session-1:run-1:1".to_owned()),
        approval_request: None,
        run_event: DesktopPublicRunEvent {
            schema_version: DESKTOP_PUBLIC_RUN_EVENT_SCHEMA_VERSION,
            session_id: "session-1".to_owned(),
            run_id: "run-1".to_owned(),
            sequence: 1,
            event,
        },
    }
}

#[test]
fn timeline_projection_keeps_conversation_text_and_drops_raw_tool_arguments() {
    let event = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "tool_call_started",
            "call": {
                "id": "call-1",
                "name": "write_file",
                "args_json": "{\"path\":\"/private/secret\"}"
            }
        }),
    );

    let timeline = event
        .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
        .expect("event should project");
    let serialized = serde_json::to_string(&timeline).expect("timeline should serialize");

    assert_eq!(timeline.kind, DesktopTimelineEventKind::ToolStarted);
    assert_eq!(timeline.tool_name.as_deref(), Some("write_file"));
    assert_eq!(timeline.item_id.as_deref(), Some("call-1"));
    assert!(!serialized.contains("private"));
    assert!(!serialized.contains("args_json"));
}

#[test]
fn approval_projection_requires_exact_guard_and_bounds_preview() {
    let mut event = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "approval_requested",
            "call": {"id": "call-1", "name": "write_file", "args_json": "{}"},
            "operation": "edit_file",
            "risk": "medium",
            "snapshot_required": true,
            "preview": {"title": "Edit file", "summary": "One change", "body": "diff"}
        }),
    );
    event.approval_request = Some(DesktopPendingApproval {
        call_id: "call-1".to_owned(),
        tool_name: "write_file".to_owned(),
        approval_request_id: "request-1".to_owned(),
        tool_call_hash: "hash-1".to_owned(),
        policy_version: "policy-1".to_owned(),
        expires_at_ms: 42,
    });

    let timeline = event
        .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
        .expect("approval should project");
    let approval = timeline.approval.expect("approval guard should remain");
    assert_eq!(approval.tool_name, "write_file");
    assert_eq!(approval.operation.as_deref(), Some("edit_file"));
    assert!(approval.snapshot_required);
    assert_eq!(approval.preview_body.as_deref(), Some("diff"));
}

#[test]
fn protocol_projection_rejects_wrong_stream_and_invalid_replay_shape() {
    let event = envelope(
        DesktopProtocolEventClass::Durable,
        json!({"type": "run_started", "prompt": "hello"}),
    );
    assert_eq!(
        event
            .clone()
            .into_timeline("workspace-1", "session-other", "run-1", "http-session-1"),
        Err(DesktopProtocolEventError::WrongStream)
    );

    let mut transient = event;
    transient.event_class = DesktopProtocolEventClass::Transient;
    assert_eq!(
        transient.into_timeline("workspace-1", "session-1", "run-1", "http-session-1"),
        Err(DesktopProtocolEventError::InvalidReplayCursor)
    );
}

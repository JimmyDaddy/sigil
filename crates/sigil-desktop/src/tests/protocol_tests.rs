use std::net::SocketAddr;

use super::*;

fn valid_server_info() -> DesktopServerInfo {
    serde_json::from_value(serde_json::json!({
        "schema_version": 5,
        "protocol_version": 2,
        "server_version": "0.0.1-alpha.5",
        "workspace_id": "workspace-safe-id",
        "bind_addr": "127.0.0.1:12345",
        "authentication": "bearer",
        "shutdown_on_stdin_close": true,
        "capabilities": {
            "session_catalog": true,
            "durable_session_reopen": true,
            "bounded_transcript_replay": true,
            "durable_event_replay": true,
            "live_events": true,
            "approval": true,
            "cancellation": true,
            "verification": true,
            "run_context": true,
            "agent_activity": true,
            "support_diagnostics": true
        }
    }))
    .expect("fixture should decode")
}

#[test]
fn server_info_requires_exact_loopback_desktop_contract() {
    let valid = valid_server_info();
    assert_eq!(
        valid.validate().expect("valid info should pass"),
        "127.0.0.1:12345"
            .parse::<SocketAddr>()
            .expect("fixture address")
    );

    let mut remote = valid.clone();
    remote.bind_addr = "192.0.2.10:12345".to_owned();
    assert!(matches!(
        remote.validate(),
        Err("desktop listener is not loopback")
    ));

    let mut missing_capability = valid;
    missing_capability.capabilities.approval = false;
    assert!(matches!(
        missing_capability.validate(),
        Err("required desktop capability is unavailable")
    ));
}

#[test]
fn exact_server_info_rejects_unknown_fields() {
    let result = serde_json::from_value::<DesktopServerInfo>(serde_json::json!({
        "schema_version": 5,
        "protocol_version": 2,
        "server_version": "0.0.1-alpha.5",
        "workspace_id": "workspace-safe-id",
        "bind_addr": "127.0.0.1:12345",
        "authentication": "bearer",
        "shutdown_on_stdin_close": true,
        "capabilities": {
            "session_catalog": true,
            "durable_session_reopen": true,
            "bounded_transcript_replay": true,
            "durable_event_replay": true,
            "live_events": true,
            "approval": true,
            "cancellation": true,
            "verification": true,
            "run_context": true,
            "agent_activity": true,
            "support_diagnostics": true
        },
        "unexpected": "drift"
    }));

    assert!(result.is_err());
}

#[test]
fn accepted_server_info_round_trips_as_frontend_safe_metadata() {
    let expected = valid_server_info();
    let encoded = serde_json::to_vec(&expected).expect("metadata should encode");
    let decoded = serde_json::from_slice::<DesktopServerInfo>(&encoded)
        .expect("encoded metadata should decode");

    assert_eq!(decoded, expected);
}

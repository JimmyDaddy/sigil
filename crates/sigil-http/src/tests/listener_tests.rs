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

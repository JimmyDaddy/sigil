use super::*;
use sigil_desktop::DesktopSessionSnapshot;

#[test]
fn workspace_identity_validation_is_strict_and_path_free() {
    assert!(valid_workspace_id("workspace-0123456789ab"));
    assert!(!valid_workspace_id("../workspace"));
    assert!(!valid_workspace_id("workspace/path"));
    assert!(!valid_workspace_id(""));
}

#[test]
fn workspace_display_name_never_returns_its_parent_path() {
    let name = workspace_display_name(Path::new("/private/canary/workspace"))
        .expect("basename should be accepted");
    assert_eq!(name, "workspace");
    assert!(!name.contains("canary"));
}

#[test]
fn projected_manager_errors_are_stable_and_path_free() {
    let projected = project_manager_error(DesktopWorkspaceManagerError::InvalidWorkspace);
    assert_eq!(projected.code, "workspace_invalid");
    assert!(!projected.message.contains('/'));
}

#[test]
fn catalog_query_validation_bounds_renderer_controlled_values() {
    let query = validate_catalog_request(DesktopCatalogRequest {
        limit: Some(30),
        query: Some("rust".to_owned()),
        state: Some(DesktopCatalogState::Ready),
        ..DesktopCatalogRequest::default()
    })
    .expect("bounded query should pass");
    assert_eq!(query.limit, Some(30));
    assert_eq!(query.query.as_deref(), Some("rust"));
    assert_eq!(query.state, Some(DesktopSessionCatalogState::Ready));

    assert!(
        validate_catalog_request(DesktopCatalogRequest {
            limit: Some(0),
            ..DesktopCatalogRequest::default()
        })
        .is_err()
    );
    assert!(
        validate_catalog_request(DesktopCatalogRequest {
            cursor: Some("x".repeat(4097)),
            ..DesktopCatalogRequest::default()
        })
        .is_err()
    );
}

#[test]
fn session_projection_drops_server_private_durable_fields() {
    let private_path = "/private/canary/session.jsonl";
    let summary = DesktopSessionSummary::from(DesktopSessionSnapshot {
        id: "http-session-1".to_owned(),
        label: Some("Conversation".to_owned()),
        run_ids: vec!["run-1".to_owned()],
        durable_session_scope_id: "durable-secret-scope".to_owned(),
        session_log_path: private_path.to_owned(),
        foreground_run_id: None,
    });
    let projection = serde_json::to_string(&summary).expect("summary should serialize");
    assert!(!projection.contains(private_path));
    assert!(!projection.contains("durable-secret-scope"));
    assert_eq!(summary.run_count, 1);
}

#[test]
fn session_open_reference_rejects_path_shaped_input() {
    assert!(validate_session_reference("session.jsonl", "session-1").is_ok());
    assert!(validate_session_reference("../session.jsonl", "session-1").is_err());
    assert!(validate_session_reference("nested/session.jsonl", "session-1").is_err());
    assert!(validate_session_reference("nested\\session.jsonl", "session-1").is_err());
}

use super::*;
use sigil_desktop::{
    DesktopSessionSnapshot, DesktopSessionTranscriptMessage, DesktopSessionTranscriptPage,
    DesktopTranscriptAssistantKind, DesktopTranscriptRole,
};

#[test]
fn workspace_identity_validation_is_strict_and_path_free() {
    assert!(valid_workspace_id("workspace-0123456789ab"));
    assert!(!valid_workspace_id("../workspace"));
    assert!(!valid_workspace_id("workspace/path"));
    assert!(!valid_workspace_id(""));
}

#[test]
fn bundled_runtime_name_cannot_collide_with_the_desktop_product_name() {
    let name = bundled_sigil_binary_name();
    assert!(name.starts_with("sigil-runtime"));
    assert_ne!(name.to_ascii_lowercase(), "sigil");
}

#[test]
fn debug_binary_resolution_prefers_the_current_developer_runtime() {
    let temp = tempfile::tempdir().expect("temporary directory should create");
    let developer = temp
        .path()
        .join(if cfg!(windows) { "sigil.exe" } else { "sigil" });
    let bundled = temp.path().join(bundled_sigil_binary_name());
    std::fs::write(&developer, b"current").expect("developer runtime should create");
    std::fs::write(&bundled, b"stale").expect("bundled runtime should create");

    assert_eq!(
        resolve_sigil_binary_from_directory(temp.path(), true),
        Some(developer)
    );
    assert_eq!(
        resolve_sigil_binary_from_directory(temp.path(), false),
        Some(bundled)
    );
}

#[test]
fn incompatible_runtime_projection_is_actionable_and_path_free() {
    let projected = project_manager_error(DesktopWorkspaceManagerError::Launch(
        DesktopLaunchError::IncompatibleServer("schema version mismatch"),
    ));

    assert_eq!(projected.code, "workspace_server_incompatible");
    assert!(projected.message.contains("out of sync"));
    assert!(!projected.message.contains('/'));
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
fn transcript_projection_drops_scope_and_preserves_safe_pagination_fields() {
    let projected = crate::ipc::DesktopTranscriptPage::from(DesktopSessionTranscriptPage {
        session_scope_id: "durable-secret-scope".to_owned(),
        total_messages: 2,
        messages: vec![DesktopSessionTranscriptMessage {
            ordinal: 2,
            message_id: "message-2".to_owned(),
            role: DesktopTranscriptRole::Assistant,
            content: Some("done".to_owned()),
            assistant_kind: Some(DesktopTranscriptAssistantKind::FinalAnswer),
            tool_name: None,
            image_attachment_count: 0,
            truncated: false,
            original_content_bytes: 4,
        }],
        next_before: Some(2),
    });
    let json = serde_json::to_value(projected).expect("transcript should serialize");

    assert_eq!(json["totalMessages"], 2);
    assert_eq!(json["messages"][0]["assistantKind"], "final_answer");
    assert_eq!(json["nextBefore"], 2);
    assert!(!json.to_string().contains("durable-secret-scope"));
}

#[test]
fn session_open_reference_rejects_path_shaped_input() {
    assert!(validate_session_reference("session.jsonl", "session-1").is_ok());
    assert!(validate_session_reference("../session.jsonl", "session-1").is_err());
    assert!(validate_session_reference("nested/session.jsonl", "session-1").is_err());
    assert!(validate_session_reference("nested\\session.jsonl", "session-1").is_err());
}

#[test]
fn verification_rerun_requires_one_bounded_exact_binding() {
    let valid = DesktopVerificationRerunInput {
        session_id: "http-session-1".to_owned(),
        request: crate::ipc::DesktopVerificationRerunBinding {
            task_id: "task_1".to_owned(),
            step_id: "verify_1".to_owned(),
            check_spec_id: "cargo-test".to_owned(),
            check_spec_hash: "check-hash".to_owned(),
            policy_hash: "policy-hash".to_owned(),
            workspace_snapshot_id: "snapshot-1".to_owned(),
        },
    };
    assert!(validate_verification_rerun(&valid).is_ok());

    let mut invalid = valid;
    invalid.request.policy_hash.clear();
    assert!(validate_verification_rerun(&invalid).is_err());
}

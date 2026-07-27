use super::*;

#[test]
fn command_shape_contains_no_path_or_authority() {
    let command = application_command(IntentCommand::Drop {
        operation_id: "operation-drop-core".to_owned(),
        stack_version: 7,
        preview_digest: format!("sha256:jcs-v1:{}", "b".repeat(64)),
    })
    .expect("command should build");
    let value = serde_json::to_value(command).expect("command should serialize");
    assert_eq!(value["action"], "execute_drop");
    assert_eq!(
        value["request"].as_object().map(serde_json::Map::len),
        Some(3)
    );
    let encoded = value.to_string();
    assert!(!encoded.contains("path"));
    assert!(!encoded.contains("authority"));
    assert!(!encoded.contains("permission"));
    assert!(!encoded.contains("policy"));
}

#[test]
fn invalid_identity_and_version_fail_before_session_lookup() {
    assert_eq!(
        application_command(IntentCommand::DropPreview {
            intent_id: "../escape".to_owned(),
            intent_version: 1,
        })
        .expect_err("path-shaped identity must fail"),
        IntentAutomationErrorCode::InvalidInvocation
    );
    assert_eq!(
        application_command(IntentCommand::DropPreview {
            intent_id: "intent-core".to_owned(),
            intent_version: 0,
        })
        .expect_err("zero version must fail"),
        IntentAutomationErrorCode::InvalidInvocation
    );
    assert!(!valid_session_id("../session"));
    assert!(valid_session_id("session-0123456789"));
}

#[test]
fn error_record_is_bounded_and_path_free() {
    let execution =
        IntentCommandExecution::error("session-1", IntentAutomationErrorCode::SessionUnavailable);
    let value = serde_json::to_value(execution.record).expect("record should serialize");
    assert_eq!(value["record_type"], "error");
    assert_eq!(value["protocol_version"], 1);
    assert_eq!(value["session_id"], "session-1");
    assert_eq!(value["error"]["code"], "session_unavailable");
    assert_eq!(value["error"]["retryable"], true);
    assert!(!value.to_string().contains('/'));
}

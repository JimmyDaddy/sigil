use serde_json::{Value, json};

use super::*;

fn request_value() -> Value {
    json!({
        "identity": { "session_scope_id": "presentation-session", "root_logical_run_id": "presentation-run",
            "source_thread_id": "main", "request_id": "presentation-request", "generation": 1,
            "source_binding_hash": "a".repeat(64) },
        "request_hash": "b".repeat(64), "source": "agent", "purpose": "clarification",
        "prompt": "Provider-authored question", "questions": [{ "id": "choice", "question": "Choose a scope",
            "required": true, "options": [], "multiple": false }],
        "allowed_actions": ["submit", "decline", "cancel_run"], "requested_at_unix_ms": 1, "status": "requested"
    })
}

#[test]
fn user_input_presentation_native_request_decodes_and_preserves_each_prompt_origin() {
    for origin in ["question_text", "host_question_group"] {
        let mut value = request_value();
        value["prompt_origin"] = json!(origin);
        let request: DesktopUserInputRequest =
            serde_json::from_value(value).expect("native request");
        let wire = serde_json::to_value(request).expect("request wire");
        assert_eq!(wire["prompt_origin"], origin);
        assert_eq!(wire["prompt"], "Provider-authored question");
    }
}

#[test]
fn user_input_presentation_native_request_omits_absent_origin_and_unknown_fields() {
    let mut value = request_value();
    value["future_metadata"] = json!({ "unconsumed": true });
    let request: DesktopUserInputRequest = serde_json::from_value(value).expect("native request");
    assert_eq!(request.prompt_origin, None);
    let wire = serde_json::to_value(request).expect("request wire");
    assert!(wire.get("prompt_origin").is_none());
    assert!(wire.get("future_metadata").is_none());
}

#[test]
fn user_input_presentation_native_tool_decodes_body_free_receipts() {
    for (decision, expected) in [
        ("submitted", DesktopUserInputDecisionKind::Submitted),
        ("declined", DesktopUserInputDecisionKind::Declined),
        ("run_cancelled", DesktopUserInputDecisionKind::RunCancelled),
    ] {
        let content: DesktopConversationDisplayContent = serde_json::from_value(json!({
            "type": "tool", "truncated": false, "original_content_bytes": 0,
            "user_input_receipt": { "decision": decision, "answered_count": 2, "future_metadata": true }
        })).expect("native tool");
        let DesktopConversationDisplayContent::Tool {
            user_input_receipt: Some(receipt),
            ..
        } = content
        else {
            panic!("typed receipt should survive decode");
        };
        assert_eq!(receipt.decision, expected);
        assert_eq!(receipt.answered_count, 2);
    }
}

#[test]
fn user_input_presentation_native_tool_keeps_receipt_optional() {
    let content: DesktopConversationDisplayContent = serde_json::from_value(json!({
        "type": "tool", "tool_name": "read_file", "output": "ordinary output",
        "truncated": false, "original_content_bytes": 15, "future_metadata": true
    }))
    .expect("native tool");
    assert!(matches!(
        content,
        DesktopConversationDisplayContent::Tool {
            user_input_receipt: None,
            ..
        }
    ));
}

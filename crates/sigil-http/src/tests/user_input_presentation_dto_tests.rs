use serde_json::{Value, json};

use super::*;

fn request_value() -> Value {
    json!({
        "identity": { "session_scope_id": "presentation-session", "root_logical_run_id": "presentation-run",
            "source_thread_id": "main", "request_id": "presentation-request", "generation": 1,
            "source_binding_hash": "a".repeat(64) },
        "request_hash": "b".repeat(64), "source": "agent", "purpose": "clarification",
        "prompt": "A provider-authored prompt stays unchanged.",
        "questions": [{ "id": "choice", "question": "Choose a scope", "required": true,
            "options": [], "multiple": false }],
        "allowed_actions": ["submit", "decline", "cancel_run"], "requested_at_unix_ms": 1,
        "status": "requested"
    })
}

#[test]
fn user_input_presentation_request_keeps_absent_origin_optional_and_ignores_unknown_fields() {
    let mut value = request_value();
    value["future_metadata"] = json!({ "unconsumed": true });
    let request: HttpUserInputRequest = serde_json::from_value(value).expect("request DTO");
    assert_eq!(request.prompt_origin, None);
    let wire = serde_json::to_value(request).expect("request wire");
    assert!(wire.get("prompt_origin").is_none());
    assert!(wire.get("future_metadata").is_none());
}

#[test]
fn user_input_presentation_request_maps_typed_origin_without_rewriting_prompt() {
    for origin in ["question_text", "host_question_group"] {
        let mut value = request_value();
        value["prompt_origin"] = json!(origin);
        let source: sigil_kernel::PublicUserInputRequestV1 =
            serde_json::from_value(value).expect("public request");
        let wire =
            serde_json::to_value(HttpUserInputRequest::from(source)).expect("HTTP projection");
        assert_eq!(wire["prompt_origin"], origin);
        assert_eq!(
            wire["prompt"],
            "A provider-authored prompt stays unchanged."
        );
    }
}

#[test]
fn user_input_presentation_tool_maps_body_free_receipt_for_each_decision() {
    for decision in ["submitted", "declined", "run_cancelled"] {
        let source: ConversationDisplayContentV1 = serde_json::from_value(json!({
            "type": "tool", "call_id": "input-call", "tool_name": "request_user_input",
            "user_input_receipt": { "decision": decision, "answered_count": 2 },
            "truncated": false, "original_content_bytes": 0
        }))
        .expect("runtime display content");
        let wire = serde_json::to_value(HttpConversationDisplayContent::from(source))
            .expect("HTTP content");
        assert_eq!(
            wire["user_input_receipt"],
            json!({ "decision": decision, "answered_count": 2 })
        );
        assert!(wire.get("output").is_none());
        assert!(wire.get("input").is_none());
    }
}

#[test]
fn user_input_presentation_ordinary_tool_omits_receipt_and_preserves_unknown_field_policy() {
    let source: ConversationDisplayContentV1 = serde_json::from_value(json!({
        "type": "tool", "tool_name": "read_file", "output": "ordinary output",
        "truncated": false, "original_content_bytes": 15, "future_metadata": true
    }))
    .expect("runtime display content");
    let wire =
        serde_json::to_value(HttpConversationDisplayContent::from(source)).expect("HTTP content");
    assert!(wire.get("user_input_receipt").is_none());
    assert_eq!(wire["output"], "ordinary output");
}

#[test]
fn user_input_presentation_openapi_exposes_optional_origin_and_typed_receipt() {
    let document = crate::http_openapi_document();
    let schemas = &document["components"]["schemas"];
    assert_eq!(
        schemas["UserInputRequest"]["properties"]["prompt_origin"]["enum"],
        json!(["question_text", "host_question_group", null])
    );
    assert!(
        !schemas["UserInputRequest"]["required"]
            .as_array()
            .expect("required request fields")
            .iter()
            .any(|field| field == "prompt_origin")
    );
    let tool = schemas["ConversationDisplayContent"]["oneOf"]
        .as_array()
        .expect("content variants")
        .iter()
        .find(|variant| variant["properties"]["type"]["const"] == "tool")
        .expect("tool variant");
    assert!(tool["properties"]["user_input_receipt"].is_object());
    assert_eq!(
        schemas["ConversationDisplayUserInputReceipt"]["properties"]["decision"]["enum"],
        json!(["submitted", "declined", "run_cancelled"])
    );
    assert_eq!(
        schemas["ConversationDisplayUserInputReceipt"]["required"],
        json!(["decision", "answered_count"])
    );
}

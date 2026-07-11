use serde_json::json;

use super::*;

#[test]
fn remote_elicitation_defaults_empty_and_opt_in_is_form_only() {
    assert_eq!(
        McpRemoteClientCapabilities::empty().wire(McpRemoteProtocolVersion::V2025_11_25),
        json!({})
    );
    let enabled = McpRemoteClientCapabilities {
        roots: true,
        form_elicitation: true,
    };
    let latest = enabled.wire(McpRemoteProtocolVersion::V2025_11_25);
    assert_eq!(latest["elicitation"], json!({"form":{}}));
    assert!(latest["elicitation"].get("url").is_none());
    assert_eq!(
        enabled.wire(McpRemoteProtocolVersion::V2025_06_18)["elicitation"],
        json!({})
    );
}

#[test]
fn remote_elicitation_validates_flat_form_before_modal_and_response() {
    let request = ValidatedMcpFormRequest::parse(&json!({
        "mode":"form",
        "message":"\u{001b}[31mChoose\u{001b}[0m\u{202e}",
        "requestedSchema":{
            "type":"object",
            "properties":{
                "kind":{"type":"string","enum":["docs","code"]},
                "count":{"type":"integer"}
            },
            "required":["kind"],
            "additionalProperties":false
        }
    }))
    .expect("bounded form");
    assert_eq!(request.fields.len(), 2);
    assert!(!request.safe_message.contains('\u{1b}'));
    assert!(!request.safe_message.contains('\u{202e}'));
    request
        .validate_response(&json!({"kind":"docs","count":2}))
        .expect("valid response");
    assert!(request.validate_response(&json!({"kind":"other"})).is_err());
    assert!(
        request
            .validate_response(&json!({"kind":"docs","extra":true}))
            .is_err()
    );
}

#[test]
fn remote_elicitation_hard_rejects_url_nested_and_unbounded_forms() {
    assert!(matches!(
        ValidatedMcpFormRequest::parse(
            &json!({"mode":"url","url":"https://secret.example/callback","requestedSchema":{"type":"object","properties":{}}})
        ),
        Err(McpStreamableHttpError::UrlElicitationUnsupported)
    ));
    assert!(ValidatedMcpFormRequest::parse(&json!({
        "requestedSchema":{"type":"object","properties":{"nested":{"type":"object","properties":{}}}}
    })).is_err());
    assert!(matches!(
        McpStreamableHttpClient::reject_server_method(
            "notifications/elicitation/complete",
            &json!({"url":"https://secret"})
        ),
        McpStreamableHttpError::CapabilityNotNegotiated
    ));
    assert!(matches!(
        McpStreamableHttpClient::reject_server_method("sampling/createMessage", &json!({})),
        McpStreamableHttpError::CapabilityNotNegotiated
    ));
}

#[test]
fn remote_elicitation_version_gates_enum_names_one_of_and_multi_select() {
    let legacy = json!({
        "message":"legacy",
        "requestedSchema":{
            "type":"object",
            "properties":{"kind":{"type":"string","enum":["d","c"],"enumNames":["Docs","Code"]}},
            "additionalProperties":false
        }
    });
    assert!(
        ValidatedMcpFormRequest::parse_for_version(&legacy, McpRemoteProtocolVersion::V2025_06_18)
            .is_ok()
    );
    assert!(
        ValidatedMcpFormRequest::parse_for_version(&legacy, McpRemoteProtocolVersion::V2025_11_25)
            .is_err()
    );

    let latest = json!({
        "mode":"form",
        "message":"latest",
        "requestedSchema":{
            "type":"object",
            "properties":{
                "kind":{"type":"string","oneOf":[{"const":"docs","title":"Docs"},{"const":"code","title":"Code"}]},
                "tags":{"type":"array","items":{"type":"string","enum":["a","b"]},"uniqueItems":true}
            },
            "additionalProperties":false
        }
    });
    let request =
        ValidatedMcpFormRequest::parse_for_version(&latest, McpRemoteProtocolVersion::V2025_11_25)
            .expect("latest form subset");
    assert_eq!(request.fields[0].kind, McpRemoteFormFieldKind::SingleSelect);
    assert_eq!(request.fields[1].kind, McpRemoteFormFieldKind::MultiSelect);
    request
        .validate_response(&json!({"kind":"docs","tags":["a","b"]}))
        .expect("valid latest response");
    assert!(
        ValidatedMcpFormRequest::parse_for_version(&latest, McpRemoteProtocolVersion::V2025_06_18)
            .is_err()
    );
}

#[test]
fn remote_elicitation_rejects_refs_property_caps_and_credential_fields() {
    let properties = (0..=MAX_FORM_PROPERTIES)
        .map(|index| (format!("field{index}"), json!({"type":"string"})))
        .collect::<serde_json::Map<_, _>>();
    for invalid in [
        json!({"requestedSchema":{"type":"object","properties":properties}}),
        json!({"requestedSchema":{"type":"object","properties":{"value":{"$ref":"#/x"}},"x":{"type":"string"}}}),
        json!({"requestedSchema":{"type":"object","properties":{"api_token":{"type":"string"}}}}),
        json!({"requestedSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["missing"]}}),
        json!({"requestedSchema":{"type":"object","properties":{"value":{"type":"string","pattern":"x".repeat(1025)}}}}),
    ] {
        assert!(
            ValidatedMcpFormRequest::parse(&invalid).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn remote_elicitation_sanitizes_osc_bidi_urls_and_blocks_credential_responses() {
    let request = ValidatedMcpFormRequest::parse(&json!({
        "message":"\u{001b}]8;;https://hidden.example\u{0007}click\u{001b}]8;;\u{0007} https://visible.example \u{202e}",
        "requestedSchema":{
            "type":"object",
            "properties":{"value":{"type":"string"}},
            "additionalProperties":false
        }
    }))
    .expect("sanitized form");
    assert!(!request.safe_message.contains("hidden.example"));
    assert!(!request.safe_message.contains("visible.example"));
    assert!(!request.safe_message.contains('\u{202e}'));
    assert!(
        request
            .validate_response(&json!({"value":"AbCdEfGhIjKlMnOpQrStUvWxYz123456"}))
            .is_err()
    );
}

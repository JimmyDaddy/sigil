use super::*;

#[test]
fn connection_id_round_trips_through_serde() {
    let id = ConnectionId::new("openai.personal-1").expect("connection id should be valid");
    let encoded = serde_json::to_string(&id).expect("connection id should serialize");
    assert_eq!(encoded, "\"openai.personal-1\"");
    assert_eq!(
        serde_json::from_str::<ConnectionId>(&encoded).expect("connection id should deserialize"),
        id
    );
}

#[test]
fn connection_id_rejects_invalid_identity_shapes() {
    for invalid in [
        "",
        "OpenAI",
        "-openai",
        "open ai",
        "openai/personal",
        "openai\\personal",
        "openai\u{202e}",
    ] {
        assert!(
            ConnectionId::new(invalid).is_err(),
            "{invalid:?} must be rejected"
        );
    }
    assert!(ConnectionId::new("a".repeat(65)).is_err());
}

#[test]
fn model_ref_trims_and_preserves_provider_model_punctuation() {
    let model_ref = ModelRef::new(
        ConnectionId::new("local").expect("connection id"),
        "  vendor/model:v1.2-test  ",
    )
    .expect("model ref should be valid");
    assert_eq!(model_ref.model_id, "vendor/model:v1.2-test");

    let encoded = serde_json::to_string(&model_ref).expect("model ref should serialize");
    assert_eq!(
        serde_json::from_str::<ModelRef>(&encoded).expect("model ref should deserialize"),
        model_ref
    );
}

#[test]
fn model_ref_rejects_terminal_unsafe_ids() {
    let connection_id = ConnectionId::new("custom").expect("connection id");
    for invalid in [
        "",
        "model\nnext",
        "model\u{1b}]8;;https://example.com\u{7}",
        "model\u{009d}",
        "model\u{202e}txt",
        "model\u{2066}txt",
    ] {
        assert!(
            ModelRef::new(connection_id.clone(), invalid).is_err(),
            "{invalid:?} must be rejected"
        );
    }
    assert!(ModelRef::new(connection_id, "m".repeat(257)).is_err());
}

#[test]
fn resolved_route_deserialization_revalidates_all_persisted_fields() {
    let route = ResolvedModelRoute::new(
        ModelRef::new(
            ConnectionId::new("anthropic-work").expect("connection id"),
            "claude-sonnet-4-5",
        )
        .expect("model ref"),
        "anthropic",
        "anthropic_messages",
        "sha256:0123456789abcdef",
    )
    .expect("resolved route");
    let encoded = serde_json::to_string(&route).expect("route should serialize");
    assert_eq!(
        serde_json::from_str::<ResolvedModelRoute>(&encoded).expect("route should deserialize"),
        route
    );

    let invalid = encoded.replace("anthropic_messages", "Anthropic Messages");
    assert!(serde_json::from_str::<ResolvedModelRoute>(&invalid).is_err());
}

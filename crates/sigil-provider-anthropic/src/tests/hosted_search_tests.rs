use serde_json::json;
use sigil_kernel::{
    HostedConstraintEnforcement, HostedCustomToolCompatibility, HostedToolKind, HostedToolLimits,
    HostedToolRequest, HostedToolSupport,
};

use super::*;

fn request(authorization_id: &str) -> HostedToolRequest {
    HostedToolRequest::new(
        authorization_id,
        HostedToolKind::WebSearch,
        HostedToolLimits::default(),
    )
    .expect("fixture request should validate")
}

#[test]
fn hosted_search_capability_matrix_is_exact_and_conservative() {
    for model in [
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-sonnet-5",
        "claude-sonnet-4-6",
        "claude-sonnet-4-5",
        "claude-sonnet-4-5-20250929",
        "claude-opus-4-5",
        "claude-opus-4-5-20251101",
        "claude-haiku-4-5",
        "claude-haiku-4-5-20251001",
    ] {
        let capability = hosted_web_search_capability(model, AnthropicHostedPlatform::ClaudeApi);
        assert_eq!(
            capability.support,
            HostedToolSupport::ServerManaged,
            "{model}"
        );
        assert_eq!(
            capability.max_uses_enforcement,
            HostedConstraintEnforcement::Hard
        );
        assert_eq!(
            capability.domain_filter_enforcement,
            HostedConstraintEnforcement::Hard
        );
        assert_eq!(
            capability.custom_tool_compatibility,
            HostedCustomToolCompatibility::Supported
        );
    }
    for model in [
        "claude-test",
        "claude-sonnet-4-20250514",
        "claude-opus-4-20250514",
        "claude-unknown-9",
        "",
    ] {
        assert!(
            !hosted_web_search_capability(model, AnthropicHostedPlatform::ClaudeApi).is_supported(),
            "{model}"
        );
    }
    assert!(
        !hosted_web_search_capability(
            "claude-sonnet-4-6",
            AnthropicHostedPlatform::UnsupportedCompatibleEndpoint,
        )
        .is_supported()
    );
}

#[test]
fn hosted_search_request_rejects_duplicate_lane() {
    let requests = vec![request("authorization-1"), request("authorization-2")];
    let error = hosted_web_search_request(&requests).expect_err("duplicate lane should fail");
    assert!(error.to_string().contains("more than one"));
}

#[test]
fn continuation_store_roundtrips_exact_blocks_live_without_persisting_carriers()
-> anyhow::Result<()> {
    let store = AnthropicHostedContinuationStore::default();
    let blocks = vec![
        json!({
            "type": "server_tool_use",
            "id": "srvtoolu_1",
            "name": "web_search",
            "input": {"query": "private exact query"}
        }),
        json!({
            "type": "web_search_tool_result",
            "tool_use_id": "srvtoolu_1",
            "content": [{
                "type": "web_search_result",
                "url": "https://example.com/?token=secret",
                "title": "private title",
                "encrypted_content": "encrypted-secret"
            }]
        }),
        json!({
            "type": "text",
            "text": "safe answer",
            "citations": [{
                "type": "web_search_result_location",
                "url": "https://example.com/?token=secret",
                "title": "private title",
                "encrypted_index": "encrypted-index-secret",
                "cited_text": "source text"
            }]
        }),
    ];
    let mut state = store.retain_blocks(blocks.clone(), "pause_turn")?;
    state.message_id = Some("assistant-message-1".to_owned());

    let durable = serde_json::to_string(&state)?;
    for secret in [
        "private exact query",
        "token=secret",
        "private title",
        "encrypted-secret",
        "encrypted-index-secret",
    ] {
        assert!(!durable.contains(secret));
    }
    assert!(matches!(
        store.resolve_for_message(&[state.clone()], "assistant-message-1")?,
        ContinuationResolution::Live(resolved) if resolved == blocks
    ));
    assert!(matches!(
        AnthropicHostedContinuationStore::default()
            .resolve_for_message(&[state], "assistant-message-1")?,
        ContinuationResolution::InterruptedOnRestart
    ));
    let debug = format!("{store:?}");
    assert!(!debug.contains("private exact query"));
    assert!(!debug.contains("encrypted-secret"));
    Ok(())
}

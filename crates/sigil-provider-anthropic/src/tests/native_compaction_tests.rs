use anyhow::Result;
use serde_json::json;
use sigil_kernel::{CompletionRequest, ModelMessage};

use super::{
    ANTHROPIC_NATIVE_COMPACTION_MIN_TRIGGER_TOKENS, AnthropicNativeCompactionOptions,
    build_paused_compaction_request, parse_paused_compaction_response,
};

fn request() -> CompletionRequest {
    CompletionRequest {
        provider_name: "anthropic".to_owned(),
        model_name: "claude-sonnet-4-6".to_owned(),
        messages: vec![ModelMessage::user("preserve this exact window")],
        tools: Vec::new(),
        temperature: None,
        max_tokens: Some(64),
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    }
}

#[test]
fn paused_native_request_uses_only_the_documented_beta_shape() -> Result<()> {
    let body = build_paused_compaction_request(
        &request(),
        1024,
        &AnthropicNativeCompactionOptions {
            trigger_input_tokens: ANTHROPIC_NATIVE_COMPACTION_MIN_TRIGGER_TOKENS,
            instructions: Some("Summarize without calling tools.".to_owned()),
        },
    )?;

    assert!(!body.stream);
    assert_eq!(body.max_tokens, 64);
    assert_eq!(
        body.context_management,
        Some(json!({
            "edits": [{
                "type": "compact_20260112",
                "trigger": {"type": "input_tokens", "value": 50_000},
                "pause_after_compaction": true,
                "instructions": "Summarize without calling tools.",
            }]
        }))
    );
    Ok(())
}

#[test]
fn paused_native_request_rejects_an_invalid_trigger_or_provider_state() {
    let too_small = AnthropicNativeCompactionOptions {
        trigger_input_tokens: ANTHROPIC_NATIVE_COMPACTION_MIN_TRIGGER_TOKENS - 1,
        instructions: None,
    };
    assert!(build_paused_compaction_request(&request(), 1024, &too_small).is_err());

    let mut with_tools = request();
    with_tools.tools.push(sigil_kernel::ToolSpec {
        name: "read_file".to_owned(),
        description: "read".to_owned(),
        input_schema: json!({"type": "object"}),
        category: sigil_kernel::ToolCategory::File,
        access: sigil_kernel::ToolAccess::Read,
        network_effect: None,
        preview: sigil_kernel::ToolPreviewCapability::None,
    });
    let options = AnthropicNativeCompactionOptions {
        trigger_input_tokens: ANTHROPIC_NATIVE_COMPACTION_MIN_TRIGGER_TOKENS,
        instructions: None,
    };
    assert!(build_paused_compaction_request(&with_tools, 1024, &options).is_err());
}

#[test]
fn paused_compaction_parser_preserves_raw_content_and_rejects_ambiguous_output() -> Result<()> {
    let payload = br#"{"id":"msg_compact_1","stop_reason":"compaction","content":[{"type":"compaction","content":"opaque summary","extension":{"keep":true}}]}"#;
    let response = parse_paused_compaction_response(payload)?;
    assert_eq!(response.response_id, "msg_compact_1");
    assert_eq!(
        response.canonical_compacted_content_json(),
        Some(r#"[{"type":"compaction","content":"opaque summary","extension":{"keep":true}}]"#)
    );
    assert!(!format!("{response:?}").contains("opaque summary"));

    let no_compaction = parse_paused_compaction_response(
        br#"{"id":"msg_normal_1","stop_reason":"end_turn","content":[{"type":"text","text":"normal"}]}"#,
    )?;
    assert!(no_compaction.canonical_compacted_content_json().is_none());

    assert!(parse_paused_compaction_response(
        br#"{"id":"msg_bad_1","stop_reason":"compaction","content":[{"type":"compaction","content":"one"},{"type":"text","text":"two"}]}"#,
    )
    .is_err());
    assert!(parse_paused_compaction_response(
        br#"{"id":"msg_bad_2","stop_reason":"compaction","content":[{"type":"compaction","content":null}]}"#,
    )
    .is_err());
    Ok(())
}

use serde_json::json;
use sigil_kernel::{
    CompletionRequest, HostedToolKind, HostedToolLimits, HostedToolRequest, ImageAttachment,
    ImageInputCapability, ImageMimeType, ModelMessage, ProviderContinuationState, ToolAccess,
    ToolCall, ToolCategory, ToolPreviewCapability, ToolSpec,
};

use super::*;

fn completion_request(messages: Vec<ModelMessage>) -> CompletionRequest {
    CompletionRequest {
        provider_name: "gemini".to_owned(),
        model_name: "gemini-test".to_owned(),
        messages,
        tools: vec![ToolSpec {
            name: "read_file".to_owned(),
            description: "Read a file".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }],
        temperature: Some(0.4),
        max_tokens: Some(512),
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: true,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    }
}

fn hosted_request() -> HostedToolRequest {
    HostedToolRequest::new(
        "auth-gemini",
        HostedToolKind::WebSearch,
        HostedToolLimits::default(),
    )
    .expect("hosted request fixture should be valid")
}

#[test]
fn build_generate_content_request_maps_system_tools_and_generation_config() -> anyhow::Result<()> {
    let request = completion_request(vec![
        ModelMessage::system("system one"),
        ModelMessage::system("system two"),
        ModelMessage::user("hello"),
    ]);

    let body = build_generate_content_request(&request)?;
    let serialized = serde_json::to_value(&body)?;

    assert_eq!(serialized["contents"][0]["role"], "user");
    assert_eq!(
        serialized["systemInstruction"]["parts"][0]["text"],
        "system one"
    );
    assert_eq!(
        serialized["tools"][0]["functionDeclarations"][0]["name"],
        "read_file"
    );
    let temperature = serialized["generationConfig"]["temperature"]
        .as_f64()
        .expect("temperature should serialize as number");
    assert!((temperature - 0.4).abs() < 0.0001);
    assert_eq!(serialized["generationConfig"]["maxOutputTokens"], 512);
    assert_eq!(serialized["store"], true);
    Ok(())
}

#[test]
fn build_generate_content_request_maps_function_call_and_response() -> anyhow::Result<()> {
    let assistant = ModelMessage::assistant(
        Some("I'll read it".to_owned()),
        vec![ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: r#"{"path":"src/lib.rs"}"#.to_owned(),
        }],
    );
    let request = completion_request(vec![
        ModelMessage::user("read"),
        assistant,
        ModelMessage::tool("call-1", r#"{"status":"ok"}"#),
    ]);

    let body = build_generate_content_request(&request)?;
    let serialized = serde_json::to_value(&body)?;

    assert_eq!(serialized["contents"][1]["role"], "model");
    assert_eq!(
        serialized["contents"][1]["parts"][1]["functionCall"]["name"],
        "read_file"
    );
    assert_eq!(
        serialized["contents"][1]["parts"][1]["functionCall"]["args"]["path"],
        "src/lib.rs"
    );
    assert_eq!(serialized["contents"][2]["role"], "user");
    assert_eq!(
        serialized["contents"][2]["parts"][0]["functionResponse"]["name"],
        "read_file"
    );
    assert_eq!(
        serialized["contents"][2]["parts"][0]["functionResponse"]["response"]["result"]["status"],
        "ok"
    );
    Ok(())
}

#[test]
fn build_generate_content_request_maps_resolved_inline_image_before_text() -> anyhow::Result<()> {
    let mut user = ModelMessage::user("inspect");
    user.image_attachments.push(ImageAttachment::from_bytes(
        "image-1",
        ImageMimeType::Png,
        1,
        1,
        vec![1, 2, 3],
    )?);
    let mut request = completion_request(vec![user]);
    request.model_name = "gemini-2.5-pro".to_owned();

    let request_body = build_generate_content_request(&request)?;
    assert!(!format!("{request_body:?}").contains("AQID"));
    let body = serde_json::to_value(request_body)?;
    assert_eq!(
        body["contents"][0]["parts"][0]["inline_data"],
        json!({"mime_type": "image/png", "data": "AQID"})
    );
    assert_eq!(body["contents"][0]["parts"][1]["text"], "inspect");
    Ok(())
}

#[test]
fn gemini_image_capability_accepts_explicit_models_and_rejects_latest_alias() {
    assert_eq!(
        gemini_image_input_capability("models/gemini-2.5-pro"),
        ImageInputCapability::Supported
    );
    assert_eq!(
        gemini_image_input_capability("gemini-flash-latest"),
        ImageInputCapability::Unsupported
    );
}

#[test]
fn build_generate_content_request_replays_matching_thought_signature() -> anyhow::Result<()> {
    let assistant = ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: r#"{"path":"src/lib.rs"}"#.to_owned(),
        }],
    );
    let assistant_id = assistant.id.clone();
    let mut request = completion_request(vec![
        ModelMessage::user("read"),
        assistant,
        ModelMessage::tool("call-1", "ok"),
    ]);
    request.continuation_states.push(ProviderContinuationState {
        provider_name: "gemini".to_owned(),
        state_kind: GEMINI_THOUGHT_SIGNATURE_STATE_KIND.to_owned(),
        message_id: Some(assistant_id),
        opaque_blob: json!({
            "tool_call_id": "call-1",
            "thought_signature": "sig-1",
        }),
    });

    let body = build_generate_content_request(&request)?;
    let serialized = serde_json::to_value(&body)?;

    assert_eq!(
        serialized["contents"][1]["parts"][0]["thoughtSignature"],
        "sig-1"
    );
    assert_eq!(
        serialized["contents"][1]["parts"][0]["functionCall"]["id"],
        "call-1"
    );
    Ok(())
}

#[test]
fn build_generate_content_request_rejects_invalid_function_args_and_orphan_result() {
    let invalid = completion_request(vec![ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: "[]".to_owned(),
        }],
    )]);
    let error = build_generate_content_request(&invalid).expect_err("array args should fail");
    assert!(error.to_string().contains("must be a JSON object"));

    let orphan = completion_request(vec![ModelMessage::tool("call-missing", "ok")]);
    let error = build_generate_content_request(&orphan).expect_err("orphan result should fail");
    assert!(error.to_string().contains("no matching tool call"));
}

#[test]
fn build_generate_content_request_maps_hosted_search_google_search_tool() -> anyhow::Result<()> {
    let mut request = completion_request(vec![ModelMessage::user("search")]);
    request.tools.clear();
    request.hosted_tools.push(hosted_request());

    let body = build_generate_content_request(&request)?;
    let serialized = serde_json::to_value(body)?;

    assert_eq!(serialized["tools"], json!([{"google_search": {}}]));
    assert!(serialized.to_string().contains("google_search"));
    assert!(!serialized.to_string().contains("functionDeclarations"));
    assert!(!serialized.to_string().contains("google_search_retrieval"));
    Ok(())
}

#[test]
fn build_generate_content_request_rejects_unsupported_hosted_search_limits_and_local_tool_mix() {
    let mut mixed = completion_request(vec![ModelMessage::user("search")]);
    mixed.model_name = "gemini-2.5-flash".to_owned();
    mixed.hosted_tools.push(hosted_request());
    let error = build_generate_content_request(&mixed).expect_err("local tool mix should fail");
    assert!(error.to_string().contains("local function declarations"));

    let mut limited = completion_request(vec![ModelMessage::user("search")]);
    limited.tools.clear();
    let hosted = HostedToolRequest::new(
        "auth-gemini-limited",
        HostedToolKind::WebSearch,
        HostedToolLimits {
            max_uses: Some(1),
            ..HostedToolLimits::default()
        },
    )
    .expect("limited hosted request fixture should be valid");
    limited.hosted_tools.push(hosted);
    let error = build_generate_content_request(&limited).expect_err("limit should fail closed");
    assert!(error.to_string().contains("does not enforce"));
}

#[test]
fn build_generate_content_request_allows_gemini_three_hosted_search_and_functions()
-> anyhow::Result<()> {
    let mut request = completion_request(vec![ModelMessage::user("search")]);
    request.model_name = "gemini-3.5-flash".to_owned();
    request.hosted_tools.push(hosted_request());

    let body = build_generate_content_request(&request)?;
    let serialized = serde_json::to_value(body)?;

    assert_eq!(serialized["tools"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        serialized["tools"][0]["functionDeclarations"][0]["name"],
        "read_file"
    );
    assert_eq!(serialized["tools"][1], json!({"google_search": {}}));
    Ok(())
}

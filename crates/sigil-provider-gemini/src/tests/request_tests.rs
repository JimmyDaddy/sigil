use serde_json::json;
use sigil_kernel::{
    CompletionRequest, ModelMessage, ToolAccess, ToolCall, ToolCategory, ToolPreviewCapability,
    ToolSpec,
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
    }
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

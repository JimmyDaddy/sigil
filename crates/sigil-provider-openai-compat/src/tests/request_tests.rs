use anyhow::Result;
use sigil_kernel::{
    CompletionRequest, ModelMessage, ToolAccess, ToolCall, ToolCategory, ToolPreviewCapability,
    ToolSpec,
};

use super::build_chat_request;

#[test]
fn build_chat_request_maps_messages_tools_and_sampling_options() -> Result<()> {
    let request = CompletionRequest {
        provider_name: "openai_compat".to_owned(),
        model_name: "gpt-test".to_owned(),
        messages: vec![
            ModelMessage::system("system prompt"),
            ModelMessage::user("hello"),
            ModelMessage::assistant(
                None,
                vec![ToolCall {
                    id: "call-1".to_owned(),
                    name: "read_file".to_owned(),
                    args_json: "{\"path\":\"src/lib.rs\"}".to_owned(),
                }],
            ),
            ModelMessage::tool("call-1", "{\"content\":\"ok\"}"),
        ],
        tools: vec![ToolSpec {
            name: "read_file".to_owned(),
            description: "read a file".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                }
            }),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }],
        temperature: Some(0.2),
        max_tokens: Some(512),
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    };

    let body = build_chat_request(&request)?;
    let serialized = serde_json::to_value(body)?;

    assert_eq!(serialized["model"], "gpt-test");
    assert_eq!(serialized["stream"], true);
    assert_eq!(serialized["stream_options"]["include_usage"], true);
    let temperature = serialized["temperature"]
        .as_f64()
        .expect("temperature should serialize as number");
    assert!((temperature - 0.2).abs() < 0.00001);
    assert_eq!(serialized["max_tokens"], 512);
    assert_eq!(serialized["tool_choice"], "auto");
    assert_eq!(serialized["messages"][0]["role"], "system");
    assert_eq!(serialized["messages"][1]["content"], "hello");
    assert_eq!(
        serialized["messages"][2]["content"],
        serde_json::Value::Null
    );
    assert_eq!(serialized["messages"][2]["tool_calls"][0]["id"], "call-1");
    assert_eq!(
        serialized["messages"][3]["tool_call_id"],
        serde_json::Value::String("call-1".to_owned())
    );
    assert_eq!(serialized["tools"][0]["type"], "function");
    assert_eq!(serialized["tools"][0]["function"]["name"], "read_file");
    Ok(())
}

#[test]
fn build_chat_request_omits_tools_and_sampling_when_absent() -> Result<()> {
    let request = CompletionRequest {
        provider_name: "openai_compat".to_owned(),
        model_name: "gpt-test".to_owned(),
        messages: vec![ModelMessage::assistant(
            Some("answer".to_owned()),
            Vec::new(),
        )],
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    };

    let body = build_chat_request(&request)?;
    let serialized = serde_json::to_value(body)?;

    assert!(serialized.get("tools").is_none());
    assert!(serialized.get("tool_choice").is_none());
    assert!(serialized.get("temperature").is_none());
    assert!(serialized.get("max_tokens").is_none());
    assert_eq!(serialized["messages"][0]["content"], "answer");
    Ok(())
}

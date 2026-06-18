use serde_json::{Value, json};
use sigil_kernel::{
    CompletionRequest, MessageRole, ModelMessage, ToolAccess, ToolCall, ToolCategory,
    ToolPreviewCapability, ToolSpec,
};

use super::*;

fn completion_request(messages: Vec<ModelMessage>) -> CompletionRequest {
    CompletionRequest {
        provider_name: "anthropic".to_owned(),
        model_name: "claude-test".to_owned(),
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
        temperature: Some(0.2),
        max_tokens: None,
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
    }
}

#[test]
fn build_messages_request_maps_system_messages_tools_and_temperature() -> anyhow::Result<()> {
    let request = completion_request(vec![
        ModelMessage::system("system one"),
        ModelMessage::system("system two"),
        ModelMessage::user("hello"),
    ]);

    let body = build_messages_request(&request, 2048)?;

    assert_eq!(body.model, "claude-test");
    assert_eq!(body.max_tokens, 2048);
    assert_eq!(body.temperature, Some(0.2));
    assert_eq!(body.system.as_deref(), Some("system one\n\nsystem two"));
    assert_eq!(body.messages[0]["role"], "user");
    assert_eq!(
        body.tools.as_ref().expect("tools should render")[0]["name"],
        "read_file"
    );
    assert_eq!(
        body.tool_choice.as_ref().expect("tool choice")["type"],
        "auto"
    );
    Ok(())
}

#[test]
fn build_messages_request_maps_assistant_tool_use_and_tool_result() -> anyhow::Result<()> {
    let assistant = ModelMessage::assistant(
        Some("I'll read it".to_owned()),
        vec![ToolCall {
            id: "toolu_1".to_owned(),
            name: "read_file".to_owned(),
            args_json: r#"{"path":"src/lib.rs"}"#.to_owned(),
        }],
    );
    let request = completion_request(vec![
        ModelMessage::user("read"),
        assistant,
        ModelMessage::tool("toolu_1", r#"{"status":"ok"}"#),
    ]);

    let body = build_messages_request(&request, 1024)?;

    let assistant_content = body.messages[1]["content"]
        .as_array()
        .expect("assistant content array");
    assert_eq!(assistant_content[0]["type"], "text");
    assert_eq!(assistant_content[1]["type"], "tool_use");
    assert_eq!(assistant_content[1]["input"]["path"], "src/lib.rs");
    assert_eq!(body.messages[2]["content"][0]["type"], "tool_result");
    assert_eq!(body.messages[2]["content"][0]["tool_use_id"], "toolu_1");
    Ok(())
}

#[test]
fn build_messages_request_merges_consecutive_tool_results() -> anyhow::Result<()> {
    let assistant = ModelMessage::assistant(
        None,
        vec![
            ToolCall {
                id: "toolu_1".to_owned(),
                name: "read_file".to_owned(),
                args_json: r#"{"path":"src/lib.rs"}"#.to_owned(),
            },
            ToolCall {
                id: "toolu_2".to_owned(),
                name: "read_file".to_owned(),
                args_json: r#"{"path":"Cargo.toml"}"#.to_owned(),
            },
        ],
    );
    let request = completion_request(vec![
        ModelMessage::user("read"),
        assistant,
        ModelMessage::tool("toolu_1", "one"),
        ModelMessage::tool("toolu_2", "two"),
    ]);

    let body = build_messages_request(&request, 1024)?;

    assert_eq!(body.messages.len(), 3);
    assert_eq!(body.messages[2]["role"], "user");
    let content = body.messages[2]["content"]
        .as_array()
        .expect("tool results should be content array");
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "tool_result");
    assert_eq!(content[0]["tool_use_id"], "toolu_1");
    assert_eq!(content[1]["type"], "tool_result");
    assert_eq!(content[1]["tool_use_id"], "toolu_2");
    Ok(())
}

#[test]
fn build_messages_request_rejects_malformed_tool_args_and_missing_result_id() {
    let mut invalid = completion_request(vec![ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "toolu_1".to_owned(),
            name: "read_file".to_owned(),
            args_json: "not-json".to_owned(),
        }],
    )]);
    let error = build_messages_request(&invalid, 1024).expect_err("invalid args should fail");
    assert!(
        error
            .to_string()
            .contains("invalid Anthropic tool call args")
    );

    invalid.messages = vec![ModelMessage {
        id: "tool".to_owned(),
        role: MessageRole::Tool,
        content: Some("ok".to_owned()),
        tool_calls: Vec::new(),
        tool_call_id: None,
    }];
    let error = build_messages_request(&invalid, 1024).expect_err("missing id should fail");
    assert!(error.to_string().contains("missing tool_call_id"));
}

#[test]
fn build_messages_request_honors_explicit_max_tokens() -> anyhow::Result<()> {
    let mut request = completion_request(vec![ModelMessage::user("hello")]);
    request.max_tokens = Some(77);

    let body = build_messages_request(&request, 2048)?;

    assert_eq!(body.max_tokens, 77);
    assert_eq!(serde_json::to_value(&body)?["max_tokens"], Value::from(77));
    Ok(())
}

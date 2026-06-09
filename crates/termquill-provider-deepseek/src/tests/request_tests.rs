use anyhow::Result;
use serde_json::{Value, json};
use termquill_kernel::{ModelMessage, ToolAccess, ToolCategory, ToolPreviewCapability, ToolSpec};

use crate::{
    config::DeepSeekProviderQuirkProfile, endpoint::DeepSeekEndpointClass,
    fim::DeepSeekFimCompletionRequest, prefix::DeepSeekPrefixCompletionRequest,
};

use super::{
    StrictToolsMode, build_chat_request, build_fim_completion_request,
    build_prefix_completion_request,
};

#[test]
fn compatible_strict_tools_route_to_beta() -> Result<()> {
    let request = termquill_kernel::CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        messages: vec![ModelMessage::user("hi")],
        tools: vec![ToolSpec {
            name: "write_file".to_owned(),
            description: "write".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type":"string"},
                    "content": {"type":"string"}
                },
                "required": ["path", "content"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::Required,
        }],
        temperature: None,
        max_tokens: None,
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
    };
    let prepared = build_chat_request(
        &request,
        None,
        StrictToolsMode::Auto,
        &DeepSeekProviderQuirkProfile::default(),
    )?;
    assert_eq!(prepared.endpoint, DeepSeekEndpointClass::Beta);
    assert_eq!(
        prepared.body.tools.as_ref().expect("tools payload missing")[0]["function"]["strict"],
        Value::Bool(true)
    );
    Ok(())
}

#[test]
fn prefix_completion_builder_marks_assistant_prefix() {
    let (endpoint, body) = build_prefix_completion_request(
        DeepSeekPrefixCompletionRequest {
            model: None,
            prompt: "summarize".to_owned(),
            assistant_prefix: "```rust\n".to_owned(),
            stop: vec!["```".to_owned()],
            reasoning_effort: None,
            traffic_partition_key: None,
        },
        "deepseek-v4-flash",
        None,
        &DeepSeekProviderQuirkProfile::default(),
    );
    assert_eq!(endpoint, DeepSeekEndpointClass::Beta);
    assert_eq!(body.messages[1]["prefix"], Value::Bool(true));
    assert_eq!(body.stop.as_ref().expect("stop missing")[0], "```");
}

#[test]
fn fim_builder_uses_explicit_suffix() {
    let request = build_fim_completion_request(
        DeepSeekFimCompletionRequest {
            model: None,
            prompt: "fn main() {\n".to_owned(),
            suffix: "\n}\n".to_owned(),
            max_tokens: Some(64),
            stop: vec!["```".to_owned()],
        },
        "deepseek-v4-pro",
    );
    assert_eq!(request.model, "deepseek-v4-pro");
    assert_eq!(request.suffix, "\n}\n");
    assert_eq!(request.max_tokens, Some(64));
    assert_eq!(request.stop.expect("stop missing")[0], "```");
}

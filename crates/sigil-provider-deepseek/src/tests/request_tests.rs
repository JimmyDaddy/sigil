use anyhow::Result;
use serde_json::{Value, json};
use sigil_kernel::{
    MessageRole, ModelMessage, ReasoningEffort, ToolAccess, ToolCategory, ToolPreviewCapability,
    ToolSpec,
};

use crate::{
    config::DeepSeekProviderQuirkProfile, endpoint::DeepSeekEndpointClass,
    fim::DeepSeekFimCompletionRequest, prefix::DeepSeekPrefixCompletionRequest,
};

use super::{
    StrictToolsMode, build_chat_request, build_fim_completion_request,
    build_prefix_completion_request, extract_user_id, extract_user_id_from_partition_key,
};

#[test]
fn compatible_strict_tools_route_to_beta() -> Result<()> {
    let request = sigil_kernel::CompletionRequest {
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

#[test]
fn build_chat_request_maps_roles_null_assistant_content_and_reasoning_effort() -> Result<()> {
    let assistant = ModelMessage {
        role: MessageRole::Assistant,
        content: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
        id: "assistant-1".to_owned(),
    };
    let tool = ModelMessage {
        role: MessageRole::Tool,
        content: Some("done".to_owned()),
        tool_calls: Vec::new(),
        tool_call_id: Some("call-1".to_owned()),
        id: "tool-1".to_owned(),
    };
    let request = sigil_kernel::CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        messages: vec![assistant, tool],
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
        reasoning_effort: Some(ReasoningEffort::High),
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: Some("workspace-123".to_owned()),
        background: false,
        store: false,
        deterministic_materialization: true,
    };

    let prepared = build_chat_request(
        &request,
        Some("workspace-123".to_owned()),
        StrictToolsMode::Off,
        &DeepSeekProviderQuirkProfile {
            strict_tools_requires_beta_endpoint: false,
            ..DeepSeekProviderQuirkProfile::default()
        },
    )?;

    assert_eq!(prepared.endpoint, DeepSeekEndpointClass::Primary);
    assert_eq!(prepared.body.messages[0]["role"], "assistant");
    assert!(prepared.body.messages[0]["content"].is_null());
    assert_eq!(prepared.body.messages[1]["role"], "tool");
    assert_eq!(prepared.body.messages[1]["tool_call_id"], "call-1");
    assert_eq!(prepared.body.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(prepared.body.user_id.as_deref(), Some("workspace-123"));
    assert!(prepared.body.tools.is_none());
    Ok(())
}

#[test]
fn prefix_completion_without_stop_uses_primary_when_quirk_disabled() {
    let (endpoint, body) = build_prefix_completion_request(
        DeepSeekPrefixCompletionRequest {
            model: Some("custom-model".to_owned()),
            prompt: "draft".to_owned(),
            assistant_prefix: "prefix".to_owned(),
            stop: Vec::new(),
            reasoning_effort: Some(ReasoningEffort::Low),
            traffic_partition_key: None,
        },
        "unused-default",
        Some("user-1".to_owned()),
        &DeepSeekProviderQuirkProfile {
            prefix_completion_requires_beta_endpoint: false,
            ..DeepSeekProviderQuirkProfile::default()
        },
    );

    assert_eq!(endpoint, DeepSeekEndpointClass::Primary);
    assert_eq!(body.model, "custom-model");
    assert!(body.stop.is_none());
    assert_eq!(body.reasoning_effort.as_deref(), Some("low"));
    assert_eq!(body.user_id.as_deref(), Some("user-1"));
}

#[test]
fn extract_user_id_supports_known_strategies_and_rejects_unknown() -> Result<()> {
    let request = sigil_kernel::CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        messages: vec![ModelMessage::user("hi")],
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: Some("partition-1".to_owned()),
        background: false,
        store: false,
        deterministic_materialization: true,
    };

    assert_eq!(
        extract_user_id(&request, Some("stable_per_end_user"))?,
        Some("partition-1".to_owned())
    );
    assert_eq!(
        extract_user_id_from_partition_key(Some("partition-1".to_owned()), Some("disabled"))?,
        None
    );
    let error = extract_user_id_from_partition_key(Some("partition-1".to_owned()), Some("weird"))
        .expect_err("unsupported strategy should fail");
    assert!(
        error
            .to_string()
            .contains("unsupported user_id strategy weird")
    );
    Ok(())
}

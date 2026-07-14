use anyhow::Result;
use sigil_kernel::{
    CompletionRequest, ModelMessage, ProviderContinuationState, ReasoningEffort, ToolAccess,
    ToolCall, ToolCategory, ToolPreviewCapability, ToolSpec,
};

use crate::{
    OPENAI_RESPONSES_OUTPUT_ITEMS_STATE_KIND,
    request::{
        OPENAI_RESPONSES_PROVIDER_NAME, build_compaction_request, build_input_token_count_request,
        build_responses_request, output_items_state,
    },
};

#[test]
fn responses_request_maps_messages_tools_and_reasoning() -> Result<()> {
    let request = CompletionRequest {
        provider_name: OPENAI_RESPONSES_PROVIDER_NAME.to_owned(),
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
            input_schema: serde_json::json!({"type":"object"}),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }],
        temperature: Some(0.2),
        max_tokens: Some(512),
        reasoning_effort: Some(ReasoningEffort::High),
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    };

    let body = serde_json::to_value(build_responses_request(&request)?)?;

    assert_eq!(body["model"], "gpt-test");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["max_output_tokens"], 512);
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(body["input"][0]["role"], "developer");
    assert_eq!(body["input"][1]["role"], "user");
    assert_eq!(body["input"][2]["type"], "function_call");
    assert_eq!(body["input"][2]["call_id"], "call-1");
    assert_eq!(body["input"][3]["type"], "function_call_output");
    assert_eq!(body["input"][3]["call_id"], "call-1");
    assert_eq!(body["tools"][0]["type"], "function");
    Ok(())
}

#[test]
fn responses_request_reuses_exact_output_items_for_the_bound_assistant_message() -> Result<()> {
    let mut assistant = ModelMessage::assistant(Some("flattened text".to_owned()), Vec::new());
    assistant.id = "assistant-1".to_owned();
    let output_items = vec![serde_json::json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "content": [{"type":"output_text", "text":"native text"}],
        "provider_extension": {"preserve": true}
    })];
    let state = ProviderContinuationState {
        provider_name: OPENAI_RESPONSES_PROVIDER_NAME.to_owned(),
        state_kind: OPENAI_RESPONSES_OUTPUT_ITEMS_STATE_KIND.to_owned(),
        message_id: Some(assistant.id.clone()),
        opaque_blob: output_items_state("resp_1", output_items.clone())?,
    };
    let mut request = simple_request(vec![assistant]);
    request.continuation_states = vec![state];

    let body = serde_json::to_value(build_responses_request(&request)?)?;

    assert_eq!(body["input"], serde_json::Value::Array(output_items));
    Ok(())
}

#[test]
fn compact_request_uses_the_same_unpruned_canonical_input_window() -> Result<()> {
    let mut assistant = ModelMessage::assistant(Some("flattened text".to_owned()), Vec::new());
    assistant.id = "assistant-1".to_owned();
    let output_items = vec![serde_json::json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "content": [{"type":"output_text", "text":"native text"}],
        "encrypted_content": "opaque"
    })];
    let mut request = simple_request(vec![ModelMessage::user("hello"), assistant.clone()]);
    request.continuation_states = vec![ProviderContinuationState {
        provider_name: OPENAI_RESPONSES_PROVIDER_NAME.to_owned(),
        state_kind: OPENAI_RESPONSES_OUTPUT_ITEMS_STATE_KIND.to_owned(),
        message_id: Some(assistant.id),
        opaque_blob: output_items_state("resp_1", output_items.clone())?,
    }];

    let compact = serde_json::to_value(build_compaction_request(&request)?)?;

    assert_eq!(compact["model"], "gpt-test");
    assert_eq!(compact["input"][1], output_items[0]);
    assert!(compact.get("stream").is_none());
    assert!(compact.get("tools").is_none());
    Ok(())
}

#[test]
fn input_token_count_request_keeps_every_prompt_bearing_responses_field() -> Result<()> {
    let mut request = simple_request(vec![ModelMessage::user("count this")]);
    request.max_tokens = Some(32_768);
    request.reasoning_effort = Some(ReasoningEffort::High);
    request.tools = vec![ToolSpec {
        name: "read_file".to_owned(),
        description: "read a file".to_owned(),
        input_schema: serde_json::json!({"type":"object"}),
        category: ToolCategory::File,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    }];

    let count = serde_json::to_value(build_input_token_count_request(&request)?)?;
    let responses = serde_json::to_value(build_responses_request(&request)?)?;

    for field in ["model", "input", "tools", "tool_choice", "reasoning"] {
        assert_eq!(
            count[field], responses[field],
            "prompt field {field} drifted"
        );
    }
    for omitted in ["stream", "store", "temperature", "max_output_tokens"] {
        assert!(
            count.get(omitted).is_none(),
            "{omitted} must not enter count wire"
        );
    }
    Ok(())
}

#[test]
fn responses_request_rejects_unknown_or_unbound_native_state() {
    let mut request = simple_request(vec![ModelMessage::assistant(
        Some("text".to_owned()),
        Vec::new(),
    )]);
    request.continuation_states = vec![ProviderContinuationState {
        provider_name: OPENAI_RESPONSES_PROVIDER_NAME.to_owned(),
        state_kind: "openai.responses.unknown".to_owned(),
        message_id: None,
        opaque_blob: serde_json::json!({}),
    }];

    let error =
        build_responses_request(&request).expect_err("unknown native state must fail closed");

    assert!(
        error
            .to_string()
            .contains("unsupported OpenAI Responses continuation state kind")
    );
}

#[test]
fn responses_request_rejects_max_reasoning_effort_instead_of_downgrading_it() {
    let mut request = simple_request(vec![ModelMessage::user("hello")]);
    request.reasoning_effort = Some(ReasoningEffort::Max);

    let error = build_responses_request(&request).expect_err("max must not be silently remapped");

    assert!(error.to_string().contains("low, medium, or high"));
}

fn simple_request(messages: Vec<ModelMessage>) -> CompletionRequest {
    CompletionRequest {
        provider_name: OPENAI_RESPONSES_PROVIDER_NAME.to_owned(),
        model_name: "gpt-test".to_owned(),
        messages,
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
    }
}

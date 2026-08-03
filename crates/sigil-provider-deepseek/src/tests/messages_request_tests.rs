use serde_json::json;
use sigil_kernel::{
    CompletionRequest, HostedToolKind, HostedToolLimits, HostedToolRequest, MessageRole,
    ModelMessage, ToolAccess, ToolCall, ToolCategory, ToolPreviewCapability, ToolSpec,
};

use super::build_messages_request;
use crate::messages_continuation::DeepSeekHostedContinuationStore;

fn hosted_request(authorization_id: &str) -> HostedToolRequest {
    HostedToolRequest::new(
        authorization_id,
        HostedToolKind::WebSearch,
        HostedToolLimits::default(),
    )
    .expect("valid hosted request")
}

fn request_with(messages: Vec<ModelMessage>, hosted: HostedToolRequest) -> CompletionRequest {
    CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        messages,
        tools: vec![ToolSpec {
            name: "read_file".to_owned(),
            description: "read a file".to_owned(),
            input_schema: json!({"type": "object"}),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }],
        temperature: None,
        max_tokens: Some(512),
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: vec![hosted],
    }
}

#[test]
fn renders_hosted_declaration_first_with_custom_tools_after() {
    let request = request_with(vec![ModelMessage::user("hi")], hosted_request("auth-1"));
    let prepared = build_messages_request(
        &request,
        &request.hosted_tools[0],
        &DeepSeekHostedContinuationStore::default(),
    )
    .expect("messages request builds");
    let tools = prepared.body.tools.as_ref().expect("tools present");
    assert_eq!(tools[0]["type"], "web_search_20250305");
    assert_eq!(tools[0]["name"], "web_search");
    assert_eq!(tools[1]["name"], "read_file");
    assert_eq!(prepared.body.model, "deepseek-v4-flash");
    assert_eq!(prepared.body.max_tokens, 512);
    assert!(prepared.body.stream);
}

#[test]
fn renders_domain_filters_and_max_uses_into_hosted_declaration() {
    let hosted = HostedToolRequest::new(
        "auth-1",
        HostedToolKind::WebSearch,
        HostedToolLimits {
            max_uses: Some(3),
            allowed_domains: vec!["example.com".to_owned()],
            blocked_domains: Vec::new(),
        },
    )
    .expect("valid hosted request");
    let request = request_with(vec![ModelMessage::user("hi")], hosted);
    let prepared = build_messages_request(
        &request,
        &request.hosted_tools[0],
        &DeepSeekHostedContinuationStore::default(),
    )
    .expect("messages request builds");
    let declaration = &prepared.body.tools.as_ref().expect("tools present")[0];
    assert_eq!(declaration["max_uses"], 3);
    assert_eq!(declaration["allowed_domains"][0], "example.com");
}

#[test]
fn maps_assistant_tool_calls_to_tool_use_and_tool_results() {
    let assistant = ModelMessage {
        role: MessageRole::Assistant,
        content: Some("calling".to_owned()),
        tool_calls: vec![ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: json!({"path": "a.rs"}).to_string(),
        }],
        tool_call_id: None,
        assistant_kind: None,
        id: "assistant-1".to_owned(),
        image_attachments: Vec::new(),
    };
    let tool = ModelMessage {
        role: MessageRole::Tool,
        content: Some("contents".to_owned()),
        tool_calls: Vec::new(),
        tool_call_id: Some("call-1".to_owned()),
        assistant_kind: None,
        id: "tool-1".to_owned(),
        image_attachments: Vec::new(),
    };
    let request = request_with(
        vec![ModelMessage::user("hi"), assistant, tool],
        hosted_request("auth-1"),
    );
    let prepared = build_messages_request(
        &request,
        &request.hosted_tools[0],
        &DeepSeekHostedContinuationStore::default(),
    )
    .expect("messages request builds");
    let messages = &prepared.body.messages;
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["role"], "user");
    let assistant_content = messages[1]["content"].as_array().expect("assistant blocks");
    assert_eq!(assistant_content[0]["type"], "text");
    assert_eq!(assistant_content[1]["type"], "tool_use");
    assert_eq!(assistant_content[1]["id"], "call-1");
    assert_eq!(assistant_content[1]["input"], json!({"path": "a.rs"}));
    let tool_content = messages[2]["content"].as_array().expect("tool blocks");
    assert_eq!(tool_content[0]["type"], "tool_result");
    assert_eq!(tool_content[0]["tool_use_id"], "call-1");
    assert_eq!(tool_content[0]["content"], "contents");
}

#[test]
fn collects_system_messages_into_system_param() {
    let system = ModelMessage {
        role: MessageRole::System,
        content: Some("be careful".to_owned()),
        tool_calls: Vec::new(),
        tool_call_id: None,
        assistant_kind: None,
        id: "system-1".to_owned(),
        image_attachments: Vec::new(),
    };
    let request = request_with(
        vec![system, ModelMessage::user("hi")],
        hosted_request("auth-1"),
    );
    let prepared = build_messages_request(
        &request,
        &request.hosted_tools[0],
        &DeepSeekHostedContinuationStore::default(),
    )
    .expect("messages request builds");
    let system = prepared.body.system.expect("system present");
    assert_eq!(system[0]["text"], "be careful");
    assert!(
        prepared
            .body
            .messages
            .iter()
            .all(|message| message["role"] != "system")
    );
}

#[test]
fn requires_max_tokens() {
    let mut request = request_with(vec![ModelMessage::user("hi")], hosted_request("auth-1"));
    request.max_tokens = None;
    let error = build_messages_request(
        &request,
        &request.hosted_tools[0],
        &DeepSeekHostedContinuationStore::default(),
    )
    .err()
    .expect("max_tokens is required on the messages path");
    assert!(error.to_string().contains("max_tokens"));
}

#[test]
fn replays_live_continuation_blocks_into_last_assistant_message() {
    let store = DeepSeekHostedContinuationStore::default();
    let assistant = ModelMessage {
        role: MessageRole::Assistant,
        content: Some("searching".to_owned()),
        tool_calls: Vec::new(),
        tool_call_id: None,
        assistant_kind: None,
        id: "assistant-1".to_owned(),
        image_attachments: Vec::new(),
    };
    let request = request_with(vec![assistant], hosted_request("auth-1"));
    let mut state = store
        .retain_blocks(
            vec![json!({
                "type": "server_tool_use",
                "id": "invoke-9",
                "name": "web_search",
                "input": {"query": "x"}
            })],
            "pause_turn",
        )
        .expect("continuation retained");
    state.message_id = Some("assistant-1".to_owned());
    let mut request = request;
    request.continuation_states = vec![state];
    let prepared = build_messages_request(&request, &request.hosted_tools[0], &store)
        .expect("messages request builds");
    let last = prepared
        .body
        .messages
        .last()
        .expect("assistant message present");
    let content = last["content"].as_array().expect("content array");
    assert!(
        content
            .iter()
            .any(|block| block["type"] == "server_tool_use" && block["id"] == "invoke-9")
    );
    assert_eq!(
        prepared
            .prior_hosted_invocations
            .get("invoke-9")
            .map(String::as_str),
        Some("auth-1")
    );
}

#[test]
fn interrupted_continuation_fails_closed() {
    let assistant = ModelMessage {
        role: MessageRole::Assistant,
        content: Some("searching".to_owned()),
        tool_calls: Vec::new(),
        tool_call_id: None,
        assistant_kind: None,
        id: "assistant-1".to_owned(),
        image_attachments: Vec::new(),
    };
    let request = request_with(vec![assistant], hosted_request("auth-1"));
    let mut state = crate::messages_continuation::DeepSeekHostedContinuationStore::default()
        .retain_blocks(vec![json!({"type": "text", "text": "x"})], "pause_turn")
        .expect("continuation retained");
    state.message_id = Some("assistant-1".to_owned());
    // Drop the store so the handle no longer resolves.
    let mut request = request;
    request.continuation_states = vec![state];
    let error = build_messages_request(
        &request,
        &request.hosted_tools[0],
        &DeepSeekHostedContinuationStore::default(),
    )
    .err()
    .expect("interrupted continuation must fail closed");
    assert!(error.to_string().contains("not resumable"));
}

use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use sigil_kernel::{
    CompletionRequest, HostedToolRequest, ImageInputCapability, MessageRole, ModelMessage,
    ToolCall, ToolSpec, validate_image_input_capability, validate_request_image_attachments,
};

use crate::{
    hosted_search::{
        ANTHROPIC_WEB_SEARCH_TOOL_TYPE, AnthropicHostedContinuationStore, ContinuationResolution,
        hosted_web_search_request,
    },
    models::AnthropicMessagesRequest,
};

pub(crate) struct PreparedAnthropicMessagesRequest {
    pub(crate) body: AnthropicMessagesRequest,
    pub(crate) prior_hosted_invocations: std::collections::BTreeMap<String, String>,
}

#[cfg(test)]
pub fn build_messages_request(
    request: &CompletionRequest,
    default_max_tokens: u32,
) -> Result<AnthropicMessagesRequest> {
    Ok(build_messages_request_with_continuations(
        request,
        default_max_tokens,
        &AnthropicHostedContinuationStore::default(),
    )?
    .body)
}

pub(crate) fn build_messages_request_with_continuations(
    request: &CompletionRequest,
    default_max_tokens: u32,
    continuation_store: &AnthropicHostedContinuationStore,
) -> Result<PreparedAnthropicMessagesRequest> {
    validate_request_image_attachments(request)?;
    validate_image_input_capability(
        anthropic_image_input_capability(&request.model_name),
        request,
    )?;
    let mut system_parts = Vec::new();
    let mut messages = Vec::new();
    let mut pending_tool_results = Vec::new();
    let mut prior_hosted_invocations = std::collections::BTreeMap::new();
    let hosted_search = hosted_web_search_request(&request.hosted_tools)?;
    for message in &request.messages {
        match message.role {
            MessageRole::System => {
                if let Some(content) = non_empty_content(message) {
                    system_parts.push(content.to_owned());
                }
            }
            MessageRole::User => {
                flush_tool_results(&mut messages, &mut pending_tool_results);
                messages.push(user_message_to_json(message)?);
            }
            MessageRole::Assistant => {
                flush_tool_results(&mut messages, &mut pending_tool_results);
                match continuation_store
                    .resolve_for_message(&request.continuation_states, &message.id)?
                {
                    ContinuationResolution::Live(blocks) => {
                        collect_prior_hosted_invocations(
                            &blocks,
                            hosted_search,
                            &mut prior_hosted_invocations,
                        );
                        messages.push(json!({"role": "assistant", "content": blocks}));
                    }
                    ContinuationResolution::Absent
                    | ContinuationResolution::InterruptedOnRestart => {
                        messages.push(assistant_message_to_json(message)?);
                    }
                }
            }
            MessageRole::Tool => pending_tool_results.push(tool_result_block(message)?),
        }
    }
    flush_tool_results(&mut messages, &mut pending_tool_results);
    let tools = anthropic_tools(&request.tools, hosted_search);

    Ok(PreparedAnthropicMessagesRequest {
        body: AnthropicMessagesRequest {
            model: request.model_name.clone(),
            messages,
            max_tokens: request.max_tokens.unwrap_or(default_max_tokens),
            stream: true,
            system: (!system_parts.is_empty()).then(|| system_parts.join("\n\n")),
            tool_choice: tools.as_ref().map(|_| json!({"type": "auto"})),
            tools,
            temperature: request.temperature,
            context_management: None,
        },
        prior_hosted_invocations,
    })
}

fn user_message_to_json(message: &ModelMessage) -> Result<Value> {
    if message.image_attachments.is_empty() {
        return Ok(json!({
            "role": "user",
            "content": non_empty_content(message).unwrap_or_default(),
        }));
    }
    let mut content = Vec::with_capacity(message.image_attachments.len() + 1);
    for attachment in &message.image_attachments {
        content.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": attachment.mime_type.as_str(),
                "data": STANDARD.encode(attachment.resolved_bytes()?),
            },
        }));
    }
    if let Some(text) = non_empty_content(message) {
        content.push(json!({"type": "text", "text": text}));
    }
    Ok(json!({"role": "user", "content": content}))
}

pub(crate) fn anthropic_image_input_capability(model_name: &str) -> ImageInputCapability {
    const ALIASES: &[&str] = &[
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-opus-4-5",
        "claude-sonnet-5",
        "claude-sonnet-4-6",
        "claude-sonnet-4-5",
        "claude-haiku-4-5",
        "claude-fable-5",
        "claude-mythos-5",
        "claude-mythos-preview",
    ];
    let model_name = model_name.trim().to_ascii_lowercase();
    if ALIASES
        .iter()
        .any(|alias| model_name == *alias || is_dated_model_id(&model_name, alias))
    {
        ImageInputCapability::Supported
    } else {
        ImageInputCapability::Unsupported
    }
}

fn is_dated_model_id(model_name: &str, alias: &str) -> bool {
    model_name
        .strip_prefix(alias)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|date| date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit()))
}

fn assistant_message_to_json(message: &ModelMessage) -> Result<Value> {
    let mut content = Vec::new();
    if let Some(text) = non_empty_content(message) {
        content.push(json!({
            "type": "text",
            "text": text,
        }));
    }
    for call in &message.tool_calls {
        content.push(tool_use_block(call)?);
    }
    Ok(json!({
        "role": "assistant",
        "content": content,
    }))
}

fn tool_use_block(call: &ToolCall) -> Result<Value> {
    Ok(json!({
        "type": "tool_use",
        "id": call.id,
        "name": call.name,
        "input": parse_tool_args(&call.args_json)?,
    }))
}

fn tool_result_block(message: &ModelMessage) -> Result<Value> {
    let tool_use_id = message
        .tool_call_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("Anthropic tool result message is missing tool_call_id"))?;
    Ok(json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": non_empty_content(message).unwrap_or_default(),
    }))
}

fn flush_tool_results(messages: &mut Vec<Value>, pending_tool_results: &mut Vec<Value>) {
    if pending_tool_results.is_empty() {
        return;
    }
    messages.push(json!({
        "role": "user",
        "content": std::mem::take(pending_tool_results),
    }));
}

fn anthropic_tools(
    tools: &[ToolSpec],
    hosted_search: Option<&HostedToolRequest>,
) -> Option<Vec<Value>> {
    if tools.is_empty() && hosted_search.is_none() {
        return None;
    }
    let mut rendered = Vec::with_capacity(tools.len() + usize::from(hosted_search.is_some()));
    if let Some(hosted_search) = hosted_search {
        let mut hosted = serde_json::Map::new();
        hosted.insert(
            "type".to_owned(),
            Value::String(ANTHROPIC_WEB_SEARCH_TOOL_TYPE.to_owned()),
        );
        hosted.insert("name".to_owned(), Value::String("web_search".to_owned()));
        if let Some(max_uses) = hosted_search.limits.max_uses {
            hosted.insert("max_uses".to_owned(), Value::from(max_uses));
        }
        if !hosted_search.limits.allowed_domains.is_empty() {
            hosted.insert(
                "allowed_domains".to_owned(),
                serde_json::to_value(&hosted_search.limits.allowed_domains)
                    .expect("validated domain filters serialize"),
            );
        }
        if !hosted_search.limits.blocked_domains.is_empty() {
            hosted.insert(
                "blocked_domains".to_owned(),
                serde_json::to_value(&hosted_search.limits.blocked_domains)
                    .expect("validated domain filters serialize"),
            );
        }
        rendered.push(Value::Object(hosted));
    }
    rendered.extend(tools.iter().map(|tool| {
        json!({
            "name": tool.name,
            "description": tool.description,
            "input_schema": tool.input_schema,
        })
    }));
    Some(rendered)
}

fn collect_prior_hosted_invocations(
    blocks: &[Value],
    hosted_search: Option<&HostedToolRequest>,
    prior: &mut std::collections::BTreeMap<String, String>,
) {
    let Some(authorization_id) = hosted_search.map(|request| request.authorization_id.as_str())
    else {
        return;
    };
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("server_tool_use")
            && block.get("name").and_then(Value::as_str) == Some("web_search")
            && let Some(id) = block.get("id").and_then(Value::as_str)
        {
            prior.insert(id.to_owned(), authorization_id.to_owned());
        }
    }
}

fn parse_tool_args(raw: &str) -> Result<Value> {
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| anyhow!("invalid Anthropic tool call args JSON: {error}"))?;
    if !value.is_object() {
        return Err(anyhow!("Anthropic tool call args must be a JSON object"));
    }
    Ok(value)
}

fn non_empty_content(message: &ModelMessage) -> Option<&str> {
    message
        .content
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
#[path = "tests/request_tests.rs"]
mod tests;

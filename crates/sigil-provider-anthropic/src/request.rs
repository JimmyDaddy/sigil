use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use sigil_kernel::{CompletionRequest, MessageRole, ModelMessage, ToolCall, ToolSpec};

use crate::models::AnthropicMessagesRequest;

pub fn build_messages_request(
    request: &CompletionRequest,
    default_max_tokens: u32,
) -> Result<AnthropicMessagesRequest> {
    let mut system_parts = Vec::new();
    let mut messages = Vec::new();
    let mut pending_tool_results = Vec::new();
    for message in &request.messages {
        match message.role {
            MessageRole::System => {
                if let Some(content) = non_empty_content(message) {
                    system_parts.push(content.to_owned());
                }
            }
            MessageRole::User => {
                flush_tool_results(&mut messages, &mut pending_tool_results);
                messages.push(json!({
                    "role": "user",
                    "content": non_empty_content(message).unwrap_or_default(),
                }));
            }
            MessageRole::Assistant => {
                flush_tool_results(&mut messages, &mut pending_tool_results);
                messages.push(assistant_message_to_json(message)?);
            }
            MessageRole::Tool => pending_tool_results.push(tool_result_block(message)?),
        }
    }
    flush_tool_results(&mut messages, &mut pending_tool_results);
    let tools = anthropic_tools(&request.tools);

    Ok(AnthropicMessagesRequest {
        model: request.model_name.clone(),
        messages,
        max_tokens: request.max_tokens.unwrap_or(default_max_tokens),
        stream: true,
        system: (!system_parts.is_empty()).then(|| system_parts.join("\n\n")),
        tool_choice: tools.as_ref().map(|_| json!({"type": "auto"})),
        tools,
        temperature: request.temperature,
    })
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

fn anthropic_tools(tools: &[ToolSpec]) -> Option<Vec<Value>> {
    if tools.is_empty() {
        return None;
    }
    Some(
        tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.input_schema,
                })
            })
            .collect(),
    )
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

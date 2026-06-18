use std::collections::BTreeMap;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use sigil_kernel::{CompletionRequest, MessageRole, ModelMessage, ToolCall, ToolSpec};

use crate::{
    mapper::GEMINI_THOUGHT_SIGNATURE_STATE_KIND,
    models::{GeminiGenerateContentRequest, GeminiGenerationConfig},
};

pub fn build_generate_content_request(
    request: &CompletionRequest,
) -> Result<GeminiGenerateContentRequest> {
    let mut system_parts = Vec::new();
    let mut contents = Vec::new();
    let mut tool_names_by_id = BTreeMap::new();
    let thought_signatures = index_thought_signatures(request);

    for message in &request.messages {
        match message.role {
            MessageRole::System => {
                if let Some(content) = non_empty_content(message) {
                    system_parts.push(json!({"text": content}));
                }
            }
            MessageRole::User => contents.push(content_with_text_role("user", message)),
            MessageRole::Assistant => {
                contents.push(assistant_message_to_content(
                    message,
                    &mut tool_names_by_id,
                    &thought_signatures,
                )?);
            }
            MessageRole::Tool => {
                contents.push(tool_result_message_to_content(message, &tool_names_by_id)?);
            }
        }
    }

    Ok(GeminiGenerateContentRequest {
        contents,
        tools: gemini_tools(&request.tools),
        system_instruction: (!system_parts.is_empty()).then(|| {
            json!({
                "role": "system",
                "parts": system_parts,
            })
        }),
        generation_config: generation_config(request),
        store: request.store.then_some(true),
    })
}

fn content_with_text_role(role: &str, message: &ModelMessage) -> Value {
    json!({
        "role": role,
        "parts": [{
            "text": non_empty_content(message).unwrap_or_default(),
        }],
    })
}

fn assistant_message_to_content(
    message: &ModelMessage,
    tool_names_by_id: &mut BTreeMap<String, String>,
    thought_signatures: &BTreeMap<(String, String), String>,
) -> Result<Value> {
    let mut parts = Vec::new();
    if let Some(content) = non_empty_content(message) {
        parts.push(json!({"text": content}));
    }
    for call in &message.tool_calls {
        tool_names_by_id.insert(call.id.clone(), call.name.clone());
        let signature = thought_signatures.get(&(message.id.clone(), call.id.clone()));
        parts.push(function_call_part(call, signature.map(String::as_str))?);
    }
    Ok(json!({
        "role": "model",
        "parts": parts,
    }))
}

fn function_call_part(call: &ToolCall, thought_signature: Option<&str>) -> Result<Value> {
    let mut part = json!({
        "functionCall": {
            "id": call.id,
            "name": call.name,
            "args": parse_tool_args(&call.args_json)?,
        }
    });
    if let Some(signature) = thought_signature
        && !signature.trim().is_empty()
    {
        part["thoughtSignature"] = Value::String(signature.to_owned());
    }
    Ok(part)
}

fn tool_result_message_to_content(
    message: &ModelMessage,
    tool_names_by_id: &BTreeMap<String, String>,
) -> Result<Value> {
    let tool_call_id = message
        .tool_call_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("Gemini tool result message is missing tool_call_id"))?;
    let tool_name = tool_names_by_id.get(tool_call_id).ok_or_else(|| {
        anyhow!("Gemini tool result has no matching tool call for {tool_call_id}")
    })?;
    Ok(json!({
        "role": "user",
        "parts": [{
            "functionResponse": {
                "id": tool_call_id,
                "name": tool_name,
                "response": {
                    "result": parse_tool_result_content(message.content.as_deref().unwrap_or_default()),
                },
            },
        }],
    }))
}

fn gemini_tools(tools: &[ToolSpec]) -> Option<Vec<Value>> {
    if tools.is_empty() {
        return None;
    }
    Some(vec![json!({
        "functionDeclarations": tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                })
            })
            .collect::<Vec<_>>(),
    })])
}

fn generation_config(request: &CompletionRequest) -> Option<GeminiGenerationConfig> {
    if request.temperature.is_none() && request.max_tokens.is_none() {
        return None;
    }
    Some(GeminiGenerationConfig {
        temperature: request.temperature,
        max_output_tokens: request.max_tokens,
    })
}

fn parse_tool_args(raw: &str) -> Result<Value> {
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| anyhow!("invalid Gemini tool call args JSON: {error}"))?;
    if !value.is_object() {
        return Err(anyhow!("Gemini tool call args must be a JSON object"));
    }
    Ok(value)
}

fn parse_tool_result_content(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

fn non_empty_content(message: &ModelMessage) -> Option<&str> {
    message
        .content
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn index_thought_signatures(request: &CompletionRequest) -> BTreeMap<(String, String), String> {
    request
        .continuation_states
        .iter()
        .filter(|state| {
            state.provider_name == "gemini"
                && state.state_kind == GEMINI_THOUGHT_SIGNATURE_STATE_KIND
        })
        .filter_map(|state| {
            let message_id = state.message_id.as_ref()?.to_owned();
            let tool_call_id = state
                .opaque_blob
                .get("tool_call_id")
                .and_then(Value::as_str)?
                .to_owned();
            let thought_signature = state
                .opaque_blob
                .get("thought_signature")
                .and_then(Value::as_str)?
                .to_owned();
            Some(((message_id, tool_call_id), thought_signature))
        })
        .collect()
}

#[cfg(test)]
#[path = "tests/request_tests.rs"]
mod tests;

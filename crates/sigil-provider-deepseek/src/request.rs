use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use sigil_kernel::{
    CompletionRequest, ImageInputCapability, MessageRole, ModelMessage, ProviderContinuationState,
    ReasoningEffort, validate_image_input_capability, validate_request_image_attachments,
};

use crate::{
    DeepSeekProviderQuirkProfile, StrictToolsMode,
    endpoint::DeepSeekEndpointClass,
    fim::DeepSeekFimCompletionRequest,
    models::{DeepSeekChatCompletionRequest, DeepSeekCompletionRequest, DeepSeekStreamOptions},
    prefix::DeepSeekPrefixCompletionRequest,
    tools::ToolSchemaDiagnostic,
    tools::prepare_tools,
};

pub struct PreparedChatRequest {
    pub endpoint: DeepSeekEndpointClass,
    pub body: DeepSeekChatCompletionRequest,
    pub tool_diagnostics: Vec<ToolSchemaDiagnostic>,
}

pub fn build_chat_request(
    request: &CompletionRequest,
    user_id: Option<String>,
    strict_tools_mode: StrictToolsMode,
    quirks: &DeepSeekProviderQuirkProfile,
) -> Result<PreparedChatRequest> {
    validate_request_image_attachments(request)?;
    validate_image_input_capability(
        deepseek_image_input_capability(&request.model_name),
        request,
    )?;
    let replay_states = index_replay_states(&request.continuation_states);
    let messages = request
        .messages
        .iter()
        .map(|message| model_message_to_json(message, replay_states.get(&message.id)))
        .collect::<Result<Vec<_>>>()?;
    let prepared_tools = prepare_tools(&request.tools, strict_tools_mode)?;
    let endpoint =
        if prepared_tools.strict_mode_enabled && quirks.strict_tools_requires_beta_endpoint {
            DeepSeekEndpointClass::Beta
        } else {
            DeepSeekEndpointClass::Primary
        };

    Ok(PreparedChatRequest {
        endpoint,
        body: DeepSeekChatCompletionRequest {
            model: request.model_name.clone(),
            messages,
            stream: true,
            max_tokens: request.max_tokens,
            stream_options: Some(DeepSeekStreamOptions {
                include_usage: true,
            }),
            tools: prepared_tools.payload,
            stop: None,
            reasoning_effort: request
                .reasoning_effort
                .as_ref()
                .map(reasoning_effort_to_string),
            user_id,
        },
        tool_diagnostics: prepared_tools.diagnostics,
    })
}

fn model_message_to_json(
    message: &ModelMessage,
    replay_state: Option<&ProviderContinuationState>,
) -> Result<Value> {
    let role = match message.role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    };

    let mut base = json!({
        "role": role,
    });

    if matches!(message.role, MessageRole::User) && !message.image_attachments.is_empty() {
        base["content"] = user_message_content(message)?;
    } else if let Some(content) = &message.content {
        base["content"] = Value::String(content.clone());
    } else if matches!(message.role, MessageRole::Assistant) {
        base["content"] = Value::Null;
    }

    if !message.tool_calls.is_empty() {
        base["tool_calls"] = Value::Array(
            message
                .tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            "arguments": call.args_json,
                        }
                    })
                })
                .collect(),
        );
    }

    if let Some(tool_call_id) = &message.tool_call_id {
        base["tool_call_id"] = Value::String(tool_call_id.clone());
    }

    if let Some(state) = replay_state
        && let Some(reasoning_content) = state
            .opaque_blob
            .get("reasoning_content")
            .and_then(Value::as_str)
    {
        base["reasoning_content"] = Value::String(reasoning_content.to_owned());
    }

    Ok(base)
}

fn user_message_content(message: &ModelMessage) -> Result<Value> {
    let mut content = Vec::with_capacity(1 + message.image_attachments.len());
    if message
        .content
        .as_deref()
        .is_some_and(|text| !text.trim().is_empty())
    {
        content.push(json!({
            "type": "text",
            "text": message.content.as_deref().unwrap_or_default(),
        }));
    }
    for attachment in &message.image_attachments {
        let encoded = STANDARD.encode(attachment.resolved_bytes()?);
        content.push(json!({
            "type": "image_url",
            "image_url": {
                "url": format!("data:{};base64,{encoded}", attachment.mime_type.as_str()),
            },
        }));
    }
    Ok(Value::Array(content))
}

/// Returns image-input support for the exact DeepSeek model IDs whose wire contract is verified.
///
/// Unknown and text-only model IDs remain unsupported so attachments never silently cross an
/// OpenAI-compatible boundary merely because the endpoint accepts chat-completions payloads.
pub(crate) fn deepseek_image_input_capability(model_name: &str) -> ImageInputCapability {
    if model_name
        .trim()
        .eq_ignore_ascii_case("deepseek-v4-flash-vision-exp")
    {
        ImageInputCapability::Supported
    } else {
        ImageInputCapability::Unsupported
    }
}

pub fn build_prefix_completion_request(
    request: DeepSeekPrefixCompletionRequest,
    default_model: &str,
    user_id: Option<String>,
    quirks: &DeepSeekProviderQuirkProfile,
) -> (DeepSeekEndpointClass, DeepSeekChatCompletionRequest) {
    let endpoint = if quirks.prefix_completion_requires_beta_endpoint {
        DeepSeekEndpointClass::Beta
    } else {
        DeepSeekEndpointClass::Primary
    };
    let body = DeepSeekChatCompletionRequest {
        model: request.model.unwrap_or_else(|| default_model.to_owned()),
        messages: vec![
            json!({
                "role": "user",
                "content": request.prompt,
            }),
            json!({
                "role": "assistant",
                "content": request.assistant_prefix,
                "prefix": true,
            }),
        ],
        stream: true,
        max_tokens: None,
        stream_options: Some(DeepSeekStreamOptions {
            include_usage: true,
        }),
        tools: None,
        stop: if request.stop.is_empty() {
            None
        } else {
            Some(request.stop)
        },
        reasoning_effort: request
            .reasoning_effort
            .as_ref()
            .map(reasoning_effort_to_string),
        user_id,
    };
    (endpoint, body)
}

pub fn build_fim_completion_request(
    request: DeepSeekFimCompletionRequest,
    default_model: &str,
) -> DeepSeekCompletionRequest {
    DeepSeekCompletionRequest {
        model: request.model.unwrap_or_else(|| default_model.to_owned()),
        prompt: request.prompt,
        suffix: request.suffix,
        stream: true,
        max_tokens: request.max_tokens,
        stop: if request.stop.is_empty() {
            None
        } else {
            Some(request.stop)
        },
    }
}

fn reasoning_effort_to_string(effort: &ReasoningEffort) -> String {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Max => "max",
    }
    .to_owned()
}

fn index_replay_states(
    states: &[ProviderContinuationState],
) -> std::collections::HashMap<String, ProviderContinuationState> {
    states
        .iter()
        .filter_map(|state| {
            state
                .message_id
                .clone()
                .map(|message_id| (message_id, state.clone()))
        })
        .collect()
}

pub fn extract_user_id(
    request: &CompletionRequest,
    strategy: Option<&str>,
) -> Result<Option<String>> {
    extract_user_id_from_partition_key(request.traffic_partition_key.clone(), strategy)
}

pub fn extract_user_id_from_partition_key(
    traffic_partition_key: Option<String>,
    strategy: Option<&str>,
) -> Result<Option<String>> {
    match strategy {
        Some("stable_per_end_user") | Some("stable_per_workspace") => Ok(traffic_partition_key),
        Some("disabled") | None => Ok(None),
        Some(other) => Err(anyhow!("unsupported user_id strategy {other}")),
    }
}

#[cfg(test)]
#[path = "tests/request_tests.rs"]
mod tests;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use termquill_kernel::{
    CompletionRequest, MessageRole, ModelMessage, ProviderContinuationState, ReasoningEffort,
};

use crate::{
    DeepSeekProviderQuirkProfile, StrictToolsMode,
    endpoint::DeepSeekEndpointClass,
    fim::DeepSeekFimCompletionRequest,
    models::{DeepSeekChatCompletionRequest, DeepSeekCompletionRequest},
    prefix::DeepSeekPrefixCompletionRequest,
    tools::prepare_tools,
};

pub struct PreparedChatRequest {
    pub endpoint: DeepSeekEndpointClass,
    pub body: DeepSeekChatCompletionRequest,
}

pub fn build_chat_request(
    request: &CompletionRequest,
    user_id: Option<String>,
    strict_tools_mode: StrictToolsMode,
    quirks: &DeepSeekProviderQuirkProfile,
) -> Result<PreparedChatRequest> {
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
            tools: prepared_tools.payload,
            stop: None,
            reasoning_effort: request
                .reasoning_effort
                .as_ref()
                .map(reasoning_effort_to_string),
            user_id,
        },
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

    if let Some(content) = &message.content {
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
mod tests {
    use anyhow::Result;
    use serde_json::{Value, json};
    use termquill_kernel::{ModelMessage, ToolSpec};

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
                read_only: false,
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
}

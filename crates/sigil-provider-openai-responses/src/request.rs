use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use sigil_kernel::{
    CompletionRequest, MessageRole, ModelMessage, ProviderContinuationState, ReasoningEffort,
    ToolSpec,
};

use crate::{
    errors::OpenAiResponsesProviderError,
    models::{
        OpenAiResponsesCompactRequest, OpenAiResponsesInputTokenCountRequest,
        OpenAiResponsesReasoning, OpenAiResponsesRequest,
    },
};

pub const OPENAI_RESPONSES_PROVIDER_NAME: &str = "openai_responses";
pub const OPENAI_RESPONSES_OUTPUT_ITEMS_STATE_KIND: &str = "openai.responses.output_items.v1";
const OUTPUT_ITEMS_STATE_SCHEMA_VERSION: u64 = 1;

pub fn build_responses_request(request: &CompletionRequest) -> Result<OpenAiResponsesRequest> {
    if request.background {
        return Err(OpenAiResponsesProviderError::BackgroundRequestsUnsupported.into());
    }
    if !request.hosted_tools.is_empty() {
        return Err(OpenAiResponsesProviderError::HostedToolsUnsupported.into());
    }
    if request.previous_response_handle.is_some() {
        return Err(OpenAiResponsesProviderError::ResponseHandlesUnsupported.into());
    }

    let output_items_by_message = index_output_item_states(&request.continuation_states)?;
    let mut input = Vec::new();
    for message in &request.messages {
        if let Some(output_items) = output_items_by_message.get(&message.id) {
            if !matches!(message.role, MessageRole::Assistant) {
                bail!("OpenAI Responses output-item state must bind an assistant message")
            }
            input.extend(output_items.iter().cloned());
        } else {
            input.extend(model_message_to_input_items(message)?);
        }
    }

    let tools = responses_tools(&request.tools);
    Ok(OpenAiResponsesRequest {
        model: request.model_name.clone(),
        input,
        stream: true,
        store: request.store,
        tool_choice: tools.as_ref().map(|_| "auto".to_owned()),
        tools,
        temperature: request.temperature,
        max_output_tokens: request.max_tokens,
        reasoning: request
            .reasoning_effort
            .as_ref()
            .map(reasoning_effort)
            .transpose()?,
    })
}

/// Materializes the exact Responses input window used by the native compact endpoint.
///
/// The compact endpoint accepts the same provider-native item window as a stateless Responses
/// request. Reusing the normal request materializer means prior assistant output items are
/// replaced by their saved native forms instead of flattened text, while no compact-output item
/// is interpreted or removed here.
pub fn build_compaction_request(
    request: &CompletionRequest,
) -> Result<OpenAiResponsesCompactRequest> {
    let responses_request = build_responses_request(request)?;
    Ok(OpenAiResponsesCompactRequest {
        model: responses_request.model,
        input: responses_request.input,
    })
}

/// Materializes the prompt-bearing part of the exact Responses request for the official
/// `/responses/input_tokens` endpoint.
///
/// The endpoint does not accept stream/store/sampling/output-reservation fields. Every accepted
/// prompt-bearing field from the normal Responses materialization is copied unchanged, so the
/// provider can bind a returned count to the same frozen target request and validate the output
/// reservation separately.
pub fn build_input_token_count_request(
    request: &CompletionRequest,
) -> Result<OpenAiResponsesInputTokenCountRequest> {
    let responses_request = build_responses_request(request)?;
    Ok(OpenAiResponsesInputTokenCountRequest {
        model: responses_request.model,
        input: responses_request.input,
        tools: responses_request.tools,
        tool_choice: responses_request.tool_choice,
        reasoning: responses_request.reasoning,
    })
}

fn index_output_item_states(
    states: &[ProviderContinuationState],
) -> Result<HashMap<String, Vec<Value>>> {
    let mut states_by_message = HashMap::new();
    for state in states {
        if state.provider_name != OPENAI_RESPONSES_PROVIDER_NAME {
            continue;
        }
        if state.state_kind != OPENAI_RESPONSES_OUTPUT_ITEMS_STATE_KIND {
            bail!("unsupported OpenAI Responses continuation state kind")
        }
        let message_id = state
            .message_id
            .as_ref()
            .filter(|message_id| !message_id.trim().is_empty())
            .context("OpenAI Responses output-item state is missing its assistant message id")?;
        let output_items = decode_output_items_state(&state.opaque_blob)?;
        if states_by_message
            .insert(message_id.clone(), output_items)
            .is_some()
        {
            bail!("duplicate OpenAI Responses output-item state for one assistant message")
        }
    }
    Ok(states_by_message)
}

pub fn output_items_state(response_id: &str, output_items: Vec<Value>) -> Result<Value> {
    if response_id.trim().is_empty() {
        bail!("OpenAI Responses completed response is missing its id")
    }
    Ok(json!({
        "schema_version": OUTPUT_ITEMS_STATE_SCHEMA_VERSION,
        "response_id": response_id,
        "output_items": output_items,
    }))
}

fn decode_output_items_state(value: &Value) -> Result<Vec<Value>> {
    let object = value
        .as_object()
        .context("OpenAI Responses output-item state must be an object")?;
    if object.len() != 3
        || !object.contains_key("schema_version")
        || !object.contains_key("response_id")
        || !object.contains_key("output_items")
    {
        bail!("OpenAI Responses output-item state has unsupported fields")
    }
    if object.get("schema_version").and_then(Value::as_u64)
        != Some(OUTPUT_ITEMS_STATE_SCHEMA_VERSION)
    {
        bail!("unsupported OpenAI Responses output-item state schema version")
    }
    if object
        .get("response_id")
        .and_then(Value::as_str)
        .is_none_or(|response_id| response_id.trim().is_empty())
    {
        bail!("OpenAI Responses output-item state is missing a response id")
    }
    object
        .get("output_items")
        .and_then(Value::as_array)
        .cloned()
        .context("OpenAI Responses output-item state is missing its output item array")
}

fn model_message_to_input_items(message: &ModelMessage) -> Result<Vec<Value>> {
    match message.role {
        MessageRole::System => Ok(vec![role_text_item(
            "developer",
            message.content.as_deref(),
        )]),
        MessageRole::User => Ok(vec![role_text_item("user", message.content.as_deref())]),
        MessageRole::Assistant => {
            let mut items = Vec::new();
            if message.content.is_some() {
                items.push(role_text_item("assistant", message.content.as_deref()));
            }
            items.extend(message.tool_calls.iter().map(|call| {
                json!({
                    "type": "function_call",
                    "call_id": call.id,
                    "name": call.name,
                    "arguments": call.args_json,
                })
            }));
            Ok(items)
        }
        MessageRole::Tool => {
            let call_id = message
                .tool_call_id
                .as_ref()
                .filter(|call_id| !call_id.trim().is_empty())
                .ok_or_else(|| anyhow!("tool message is missing its OpenAI Responses call id"))?;
            Ok(vec![json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": message.content.as_deref().unwrap_or_default(),
            })])
        }
    }
}

fn role_text_item(role: &str, text: Option<&str>) -> Value {
    json!({
        "role": role,
        "content": [{
            "type": if role == "assistant" { "output_text" } else { "input_text" },
            "text": text.unwrap_or_default(),
        }],
    })
}

fn responses_tools(tools: &[ToolSpec]) -> Option<Vec<Value>> {
    if tools.is_empty() {
        return None;
    }
    Some(
        tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                })
            })
            .collect(),
    )
}

fn reasoning_effort(effort: &ReasoningEffort) -> Result<OpenAiResponsesReasoning> {
    let effort = match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Max => {
            return Err(OpenAiResponsesProviderError::UnsupportedReasoningEffort.into());
        }
    };
    Ok(OpenAiResponsesReasoning {
        effort: effort.to_owned(),
    })
}

#[cfg(test)]
#[path = "tests/request_tests.rs"]
mod tests;

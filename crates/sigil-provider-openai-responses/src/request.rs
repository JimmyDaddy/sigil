use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use sigil_kernel::{
    CompletionRequest, ImageInputCapability, MessageRole, ModelMessage, ProviderContinuationState,
    ReasoningEffort, ToolSpec, canonicalize_cache_stable_json,
    strip_request_image_attachments_for_compaction, validate_image_input_capability,
    validate_request_image_attachments,
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
const PROMPT_CACHE_LAYOUT_VERSION: &str = "cache_aware_v3";
const PROMPT_CACHE_SHARD_COUNT: u8 = 16;

pub fn build_responses_request(request: &CompletionRequest) -> Result<OpenAiResponsesRequest> {
    Ok(build_responses_request_with_cache_routing(request, false)?.0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenAiLogicalCachePlan {
    pub(crate) a0_input_end: Option<usize>,
    pub(crate) a2_input_end: Option<usize>,
    pub(crate) prompt_cache_key: String,
}

pub(crate) fn build_responses_request_with_cache_routing(
    request: &CompletionRequest,
    enabled: bool,
) -> Result<(OpenAiResponsesRequest, Option<OpenAiLogicalCachePlan>)> {
    validate_request_image_attachments(request)?;
    validate_image_input_capability(
        openai_responses_image_input_capability(&request.model_name),
        request,
    )?;
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
    let leading_system_count = request
        .messages
        .iter()
        .take_while(|message| message.role == MessageRole::System)
        .count();
    let active_turn_start = request
        .messages
        .iter()
        .rposition(|message| message.role == MessageRole::User);
    let mut a0_input_end = None;
    let mut a2_input_end = None;
    for (message_index, message) in request.messages.iter().enumerate() {
        if active_turn_start == Some(message_index) && !input.is_empty() {
            a2_input_end = Some(input.len());
        }
        if let Some(output_items) = output_items_by_message.get(&message.id) {
            if !matches!(message.role, MessageRole::Assistant) {
                bail!("OpenAI Responses output-item state must bind an assistant message")
            }
            input.extend(output_items.iter().cloned());
        } else {
            input.extend(model_message_to_input_items(message)?);
        }
        if leading_system_count == message_index + 1 {
            a0_input_end = Some(input.len());
        }
    }

    let tools = responses_tools(&request.tools)?;
    let reasoning = request
        .reasoning_effort
        .as_ref()
        .map(reasoning_effort)
        .transpose()?;
    let logical_cache_plan = if enabled {
        prompt_cache_key(request)?.map(|prompt_cache_key| OpenAiLogicalCachePlan {
            a0_input_end,
            a2_input_end: a2_input_end.filter(|end| Some(*end) != a0_input_end),
            prompt_cache_key,
        })
    } else {
        None
    };
    let prompt_cache_key = logical_cache_plan
        .as_ref()
        .map(|plan| plan.prompt_cache_key.clone());
    Ok((
        OpenAiResponsesRequest {
            model: request.model_name.clone(),
            input,
            stream: true,
            store: request.store,
            include: (reasoning.is_some() && !request.store)
                .then(|| vec!["reasoning.encrypted_content".to_owned()]),
            tool_choice: tools.as_ref().map(|_| "auto".to_owned()),
            tools,
            temperature: request.temperature,
            max_output_tokens: request.max_tokens,
            reasoning,
            prompt_cache_key,
            // Extended retention is incompatible with some zero-retention policies. Sigil leaves the
            // provider default in force until the connection owns an explicit retention decision.
            prompt_cache_retention: None,
        },
        logical_cache_plan,
    ))
}

fn prompt_cache_key(request: &CompletionRequest) -> Result<Option<String>> {
    let Some(tenant_partition) = request
        .traffic_partition_key
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    let Some(session_seed) = request
        .messages
        .iter()
        .find(|message| message.role != MessageRole::System)
        .or_else(|| request.messages.first())
        .map(|message| message.id.as_str())
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(tenant_partition.as_bytes())
        .map_err(|_| anyhow!("failed to initialize OpenAI prompt-cache HMAC"))?;
    for (label, value) in [
        ("domain", "sigil-openai-prompt-cache-key-v1"),
        ("session", session_seed),
        ("route", OPENAI_RESPONSES_PROVIDER_NAME),
        ("model", request.model_name.as_str()),
        ("layout", PROMPT_CACHE_LAYOUT_VERSION),
    ] {
        mac.update(&(label.len() as u64).to_be_bytes());
        mac.update(label.as_bytes());
        mac.update(&(value.len() as u64).to_be_bytes());
        mac.update(value.as_bytes());
    }
    let digest = format!("{:x}", mac.finalize().into_bytes());
    let shard = u8::from_str_radix(&digest[..2], 16)
        .map_err(|error| anyhow!("invalid OpenAI prompt-cache HMAC shard: {error}"))?
        % PROMPT_CACHE_SHARD_COUNT;
    Ok(Some(format!("s3v3-{shard:x}-{}", &digest[..56])))
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
    let mut compactable = request.clone();
    strip_request_image_attachments_for_compaction(&mut compactable);
    let responses_request = build_responses_request(&compactable)?;
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
        MessageRole::User => Ok(vec![user_input_item(message)?]),
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

fn user_input_item(message: &ModelMessage) -> Result<Value> {
    let mut content = Vec::with_capacity(1 + message.image_attachments.len());
    if message.image_attachments.is_empty()
        || message
            .content
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
    {
        content.push(json!({
            "type": "input_text",
            "text": message.content.as_deref().unwrap_or_default(),
        }));
    }
    for attachment in &message.image_attachments {
        let encoded = STANDARD.encode(attachment.resolved_bytes()?);
        content.push(json!({
            "type": "input_image",
            "image_url": format!("data:{};base64,{encoded}", attachment.mime_type.as_str()),
            "detail": "high",
        }));
    }
    Ok(json!({"role": "user", "content": content}))
}

pub(crate) fn openai_responses_image_input_capability(model_name: &str) -> ImageInputCapability {
    const ALIASES: &[&str] = &[
        "gpt-5.6",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-5.5",
        "gpt-5.5-pro",
        "gpt-5.4",
        "gpt-5.4-pro",
        "gpt-5.4-mini",
        "gpt-5.4-nano",
        "gpt-5.3-codex",
        "gpt-5.2",
        "gpt-5.2-pro",
        "gpt-5.1",
        "gpt-5",
        "gpt-5-pro",
        "gpt-5-mini",
        "gpt-5-nano",
        "gpt-4.1",
        "gpt-4.1-mini",
        "gpt-4.1-nano",
        "gpt-4o",
        "gpt-4o-mini",
        "o3",
        "o3-pro",
        "o4-mini",
    ];
    let model_name = model_name.trim().to_ascii_lowercase();
    if ALIASES
        .iter()
        .any(|alias| model_name == *alias || is_dated_snapshot_of(&model_name, alias))
    {
        ImageInputCapability::Supported
    } else {
        ImageInputCapability::Unsupported
    }
}

fn is_dated_snapshot_of(model_name: &str, alias: &str) -> bool {
    let Some(snapshot) = model_name
        .strip_prefix(alias)
        .and_then(|rest| rest.strip_prefix('-'))
    else {
        return false;
    };
    let bytes = snapshot.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

fn responses_tools(tools: &[ToolSpec]) -> Result<Option<Vec<Value>>> {
    if tools.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        tools
            .iter()
            .map(|tool| {
                Ok(json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": canonicalize_cache_stable_json(&tool.input_schema)?,
                }))
            })
            .collect::<Result<Vec<_>>>()?,
    ))
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

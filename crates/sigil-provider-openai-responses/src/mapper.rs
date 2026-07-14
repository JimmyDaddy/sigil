use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use sigil_kernel::{ProviderChunk, ProviderContinuationState, ToolCall, UsageStats};

use crate::request::{
    OPENAI_RESPONSES_OUTPUT_ITEMS_STATE_KIND, OPENAI_RESPONSES_PROVIDER_NAME, output_items_state,
};

#[derive(Debug)]
struct ActiveFunctionCall {
    call_id: String,
    name: String,
    arguments: String,
}

/// Provider-local Responses event mapper.
///
/// The mapper only interprets the few fields required to surface text, tool calls, usage and
/// terminal status. The complete `response.output` array is otherwise retained as opaque JSON so
/// a later stateless compact request can reuse it without provider-item pruning or reconstruction.
pub struct StreamMapper {
    active_function_calls: HashMap<String, ActiveFunctionCall>,
    completed: bool,
}

impl StreamMapper {
    pub fn new() -> Self {
        Self {
            active_function_calls: HashMap::new(),
            completed: false,
        }
    }

    pub fn is_completed(&self) -> bool {
        self.completed
    }

    pub fn map_event(&mut self, event: &str, payload: Value) -> Result<Vec<ProviderChunk>> {
        if self.completed {
            bail!("OpenAI Responses stream emitted an event after response.completed")
        }
        match event {
            "response.output_text.delta" => string_field(&payload, "delta")
                .map(|delta| vec![ProviderChunk::TextDelta(delta.to_owned())]),
            "response.reasoning_text.delta" => string_field(&payload, "delta")
                .map(|delta| vec![ProviderChunk::ReasoningDelta(delta.to_owned())]),
            "response.reasoning_summary_text.delta" => string_field(&payload, "delta")
                .map(|delta| vec![ProviderChunk::ReasoningSummaryDelta(delta.to_owned())]),
            "response.output_item.added" => self.map_output_item_added(payload),
            "response.function_call_arguments.delta" => self.map_function_arguments_delta(payload),
            "response.output_item.done" => self.map_output_item_done(payload),
            "response.completed" => self.map_completed(payload),
            "response.failed" | "error" => Err(response_error(event, &payload)),
            _ => Ok(Vec::new()),
        }
    }

    fn map_output_item_added(&mut self, payload: Value) -> Result<Vec<ProviderChunk>> {
        let item = object_field(&payload, "item")?;
        if string_field(item, "type")? != "function_call" {
            return Ok(Vec::new());
        }
        let item_id = required_string(item, "id")?;
        let call_id = required_string(item, "call_id")?;
        let name = required_string(item, "name")?;
        if self
            .active_function_calls
            .insert(
                item_id.to_owned(),
                ActiveFunctionCall {
                    call_id: call_id.to_owned(),
                    name: name.to_owned(),
                    arguments: String::new(),
                },
            )
            .is_some()
        {
            bail!("OpenAI Responses stream reused a live function-call item id")
        }
        Ok(vec![ProviderChunk::ToolCallStart {
            id: call_id.to_owned(),
            name: name.to_owned(),
        }])
    }

    fn map_function_arguments_delta(&mut self, payload: Value) -> Result<Vec<ProviderChunk>> {
        let item_id = required_string(&payload, "item_id")?;
        let delta = string_field(&payload, "delta")?;
        let active = self.active_function_calls.get_mut(item_id).with_context(|| {
            format!(
                "OpenAI Responses argument delta references unknown function-call item {item_id}"
            )
        })?;
        active.arguments.push_str(delta);
        Ok(vec![ProviderChunk::ToolCallArgsDelta {
            id: active.call_id.clone(),
            delta: delta.to_owned(),
        }])
    }

    fn map_output_item_done(&mut self, payload: Value) -> Result<Vec<ProviderChunk>> {
        let item = object_field(&payload, "item")?;
        if string_field(item, "type")? != "function_call" {
            return Ok(Vec::new());
        }
        let item_id = required_string(item, "id")?;
        let call_id = required_string(item, "call_id")?;
        let name = required_string(item, "name")?;
        let arguments = string_field(item, "arguments")?;
        let active = self
            .active_function_calls
            .remove(item_id)
            .with_context(|| {
                format!("OpenAI Responses completed an unknown function-call item {item_id}")
            })?;
        if active.call_id != call_id || active.name != name {
            bail!("OpenAI Responses completed function-call identity drifted")
        }
        let suffix = arguments
            .strip_prefix(&active.arguments)
            .context("OpenAI Responses completed function-call arguments diverged from stream")?;
        let mut chunks = Vec::new();
        if !suffix.is_empty() {
            chunks.push(ProviderChunk::ToolCallArgsDelta {
                id: active.call_id.clone(),
                delta: suffix.to_owned(),
            });
        }
        chunks.push(ProviderChunk::ToolCallComplete(ToolCall {
            id: active.call_id,
            name: active.name,
            args_json: arguments.to_owned(),
        }));
        Ok(chunks)
    }

    fn map_completed(&mut self, payload: Value) -> Result<Vec<ProviderChunk>> {
        if !self.active_function_calls.is_empty() {
            bail!("OpenAI Responses completed before every function-call item was finalized")
        }
        let response = object_field(&payload, "response")?;
        if string_field(response, "status")? != "completed" {
            bail!("OpenAI Responses completed event does not contain completed status")
        }
        let response_id = required_string(response, "id")?;
        let output_items = response
            .get("output")
            .and_then(Value::as_array)
            .cloned()
            .context("OpenAI Responses completed event is missing its output item array")?;

        let mut chunks = Vec::new();
        if let Some(usage) = response.get("usage") {
            chunks.push(ProviderChunk::Usage(map_usage(usage, response)?));
        }
        chunks.push(ProviderChunk::ContinuationState(
            ProviderContinuationState {
                provider_name: OPENAI_RESPONSES_PROVIDER_NAME.to_owned(),
                state_kind: OPENAI_RESPONSES_OUTPUT_ITEMS_STATE_KIND.to_owned(),
                message_id: None,
                opaque_blob: output_items_state(response_id, output_items)?,
            },
        ));
        chunks.push(ProviderChunk::Done);
        self.completed = true;
        Ok(chunks)
    }
}

fn map_usage(usage: &Value, response: &Value) -> Result<UsageStats> {
    let input_tokens = required_u64(usage, "input_tokens")?;
    let output_tokens = required_u64(usage, "output_tokens")?;
    let cache_hit_tokens = usage
        .get("input_tokens_details")
        .and_then(Value::as_object)
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    Ok(UsageStats {
        prompt_tokens: input_tokens,
        completion_tokens: output_tokens,
        cache_hit_tokens,
        cache_miss_tokens: input_tokens.saturating_sub(cache_hit_tokens),
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: response
            .get("system_fingerprint")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    })
}

fn response_error(event: &str, payload: &Value) -> anyhow::Error {
    let message = payload
        .get("error")
        .and_then(Value::as_object)
        .and_then(|error| error.get("message"))
        .or_else(|| payload.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("no provider error message");
    anyhow::anyhow!("OpenAI Responses stream {event}: {message}")
}

fn object_field<'a>(value: &'a Value, field: &str) -> Result<&'a Value> {
    value
        .get(field)
        .filter(|value| value.is_object())
        .with_context(|| format!("OpenAI Responses event is missing object field {field}"))
}

fn string_field<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("OpenAI Responses event is missing string field {field}"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    let value = string_field(value, field)?;
    if value.trim().is_empty() {
        bail!("OpenAI Responses event has an empty {field}")
    }
    Ok(value)
}

fn required_u64(value: &Value, field: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .with_context(|| format!("OpenAI Responses usage is missing integer field {field}"))
}

#[cfg(test)]
#[path = "tests/mapper_tests.rs"]
mod tests;

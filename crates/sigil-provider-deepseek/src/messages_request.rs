use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use sigil_kernel::{
    CompletionRequest, HostedToolRequest, ImageInputCapability, MessageRole,
    validate_image_input_capability, validate_request_image_attachments,
};

use crate::compaction_token_profile::DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_OUTPUT_TOKENS;
use crate::hosted_search::DEEPSEEK_WEB_SEARCH_TOOL_TYPE;
use crate::messages_continuation::{ContinuationResolution, DeepSeekHostedContinuationStore};
use crate::messages_models::DeepSeekMessagesRequest;

pub struct PreparedMessagesRequest {
    pub body: DeepSeekMessagesRequest,
    /// Server-tool invocations already present in the rendered messages (from a
    /// replayed prior hosted turn), keyed by invocation id.
    pub prior_hosted_invocations: BTreeMap<String, String>,
}

/// Builds an Anthropic-compatible Messages request carrying the hosted
/// web-search declaration plus any ordinary custom tools.
///
/// # Errors
///
/// Returns an error when the request cannot be rendered into the Messages wire
/// shape or a hosted continuation must be replayed but cannot be resolved.
pub fn build_messages_request(
    request: &CompletionRequest,
    hosted: &HostedToolRequest,
    continuations: &DeepSeekHostedContinuationStore,
) -> Result<PreparedMessagesRequest> {
    validate_request_image_attachments(request)?;
    validate_image_input_capability(ImageInputCapability::Unsupported, request)?;
    // The kernel CompletionRequest contract treats max_tokens as optional and the chat-completions
    // path omits it (server-side default), but the Messages wire requires an explicit cap. The
    // agent run boundary normally resolves the canonical DeepSeek V4 output cap via
    // `Provider::default_max_output_tokens`; this fallback is the last line of defense for
    // callers that bypass the agent loop.
    let max_tokens = request
        .max_tokens
        .unwrap_or(DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_OUTPUT_TOKENS);

    let mut messages = Vec::new();
    let mut system_blocks = Vec::new();
    let mut pending_tool_results: Vec<Value> = Vec::new();
    let mut last_assistant: Option<(usize, String)> = None;

    for message in &request.messages {
        match message.role {
            MessageRole::System => {
                if let Some(content) = &message.content {
                    system_blocks.push(json!({ "type": "text", "text": content }));
                }
            }
            MessageRole::User => {
                flush_tool_results(&mut messages, &mut pending_tool_results)?;
                let mut blocks = Vec::new();
                if let Some(content) = &message.content {
                    blocks.push(json!({ "type": "text", "text": content }));
                }
                messages.push(json!({ "role": "user", "content": blocks }));
            }
            MessageRole::Assistant => {
                flush_tool_results(&mut messages, &mut pending_tool_results)?;
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(content) = &message.content {
                    blocks.push(json!({ "type": "text", "text": content }));
                }
                for call in &message.tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": parse_tool_args(&call.args_json),
                    }));
                }
                last_assistant = Some((messages.len(), message.id.clone()));
                messages.push(json!({ "role": "assistant", "content": blocks }));
            }
            MessageRole::Tool => {
                let Some(tool_call_id) = &message.tool_call_id else {
                    bail!("DeepSeek tool message is missing its tool call id");
                };
                pending_tool_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": tool_call_id,
                    "content": message.content.clone().unwrap_or_default(),
                }));
            }
        }
    }
    flush_tool_results(&mut messages, &mut pending_tool_results)?;

    if let Some((index, message_id)) = last_assistant {
        match continuations.resolve_for_message(&request.continuation_states, &message_id)? {
            ContinuationResolution::Live(blocks) => append_blocks(&mut messages, index, blocks)?,
            ContinuationResolution::InterruptedOnRestart => {
                bail!("DeepSeek hosted web-search continuation is not resumable after restart");
            }
            ContinuationResolution::Absent => {}
        }
    }

    let prior_hosted_invocations =
        collect_prior_hosted_invocations(&messages, hosted.authorization_id.as_str());

    let mut tools = Vec::new();
    tools.push(render_hosted_declaration(hosted)?);
    for tool in &request.tools {
        tools.push(json!({
            "name": tool.name,
            "description": tool.description,
            "input_schema": tool.input_schema,
        }));
    }

    let system = if system_blocks.is_empty() {
        None
    } else {
        Some(Value::Array(system_blocks))
    };

    Ok(PreparedMessagesRequest {
        body: DeepSeekMessagesRequest {
            model: request.model_name.clone(),
            messages,
            max_tokens,
            stream: true,
            system,
            tools: Some(tools),
        },
        prior_hosted_invocations,
    })
}

fn render_hosted_declaration(hosted: &HostedToolRequest) -> Result<Value> {
    let mut declaration = serde_json::Map::new();
    declaration.insert(
        "type".to_owned(),
        Value::String(DEEPSEEK_WEB_SEARCH_TOOL_TYPE.to_owned()),
    );
    declaration.insert("name".to_owned(), Value::String("web_search".to_owned()));
    if let Some(max_uses) = hosted.limits.max_uses {
        declaration.insert("max_uses".to_owned(), Value::from(max_uses));
    }
    if !hosted.limits.allowed_domains.is_empty() {
        declaration.insert(
            "allowed_domains".to_owned(),
            serde_json::to_value(&hosted.limits.allowed_domains)
                .expect("validated domain filters serialize"),
        );
    }
    if !hosted.limits.blocked_domains.is_empty() {
        declaration.insert(
            "blocked_domains".to_owned(),
            serde_json::to_value(&hosted.limits.blocked_domains)
                .expect("validated domain filters serialize"),
        );
    }
    Ok(Value::Object(declaration))
}

fn parse_tool_args(raw: &str) -> Value {
    serde_json::from_str::<Value>(raw)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn flush_tool_results(messages: &mut Vec<Value>, pending: &mut Vec<Value>) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    messages.push(json!({
        "role": "user",
        "content": std::mem::take(pending),
    }));
    Ok(())
}

fn append_blocks(messages: &mut [Value], index: usize, blocks: Vec<Value>) -> Result<()> {
    let Some(message) = messages.get_mut(index) else {
        bail!("DeepSeek hosted continuation targets a missing assistant message");
    };
    let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
        bail!("DeepSeek hosted continuation targets a non-array assistant content");
    };
    content.extend(blocks);
    Ok(())
}

fn collect_prior_hosted_invocations(
    messages: &[Value],
    authorization_id: &str,
) -> BTreeMap<String, String> {
    let mut prior = BTreeMap::new();
    for message in messages {
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("server_tool_use")
                && let Some(id) = block.get("id").and_then(Value::as_str)
            {
                prior.insert(id.to_owned(), authorization_id.to_owned());
            }
        }
    }
    prior
}

#[cfg(test)]
#[path = "tests/messages_request_tests.rs"]
mod tests;

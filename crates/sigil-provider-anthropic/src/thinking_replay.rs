//! Durable, message-bound reasoning replay for the official DeepSeek Messages protocol.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sigil_kernel::{CompletionRequest, MessageRole, ProviderContinuationState};

use crate::models::AnthropicMessagesRequest;

pub(crate) const STATE_KIND: &str = "anthropic.deepseek_thinking_replay.v1";

pub(crate) fn into_state(model: &str, blocks: Vec<Value>) -> Result<ProviderContinuationState> {
    let opaque_blob = json!({"schema_version":1,"model":model,"blocks":blocks});
    ensure!(
        (serde_json::to_vec(&opaque_blob)?.len() as u64)
            <= sigil_kernel::MAX_PROVIDER_CONTINUATION_PAYLOAD_BYTES,
        "Messages thinking continuation exceeds the durable payload limit"
    );
    Ok(ProviderContinuationState {
        provider_name: "anthropic".to_owned(),
        state_kind: STATE_KIND.to_owned(),
        message_id: None,
        opaque_blob,
    })
}

pub(crate) fn apply(
    body: &mut AnthropicMessagesRequest,
    request: &CompletionRequest,
) -> Result<()> {
    let source = request
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant);
    let wire = body
        .messages
        .iter_mut()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"));
    for (message, wire) in source.zip(wire) {
        let state = request.continuation_states.iter().rev().find(|state| {
            state.provider_name == "anthropic"
                && state.state_kind == STATE_KIND
                && state.message_id.as_deref() == Some(message.id.as_str())
        });
        let Some(state) = state else {
            ensure!(
                message.tool_calls.is_empty(),
                "Messages tool continuation is missing its durable thinking source"
            );
            continue;
        };
        ensure!(
            state
                .opaque_blob
                .get("schema_version")
                .and_then(Value::as_u64)
                == Some(1)
                && state.opaque_blob.get("model").and_then(Value::as_str)
                    == Some(request.model_name.as_str()),
            "Messages thinking continuation does not match the current model and schema"
        );
        ensure!(
            (serde_json::to_vec(&state.opaque_blob)?.len() as u64)
                <= sigil_kernel::MAX_PROVIDER_CONTINUATION_PAYLOAD_BYTES,
            "Messages thinking continuation exceeds the durable payload limit"
        );
        let blocks = state
            .opaque_blob
            .get("blocks")
            .and_then(Value::as_array)
            .context("Messages thinking continuation has no blocks")?;
        ensure!(
            !blocks.is_empty(),
            "Messages thinking continuation has no blocks"
        );
        let mut content = Vec::with_capacity(blocks.len());
        for block in blocks {
            ensure!(
                block.get("type").and_then(Value::as_str) == Some("thinking"),
                "Messages thinking continuation contains another block type"
            );
            let thinking = block
                .get("thinking")
                .and_then(Value::as_str)
                .context("Messages thinking block is missing text")?;
            let signature = block
                .get("signature")
                .and_then(Value::as_str)
                .unwrap_or_default();
            content.push(json!({"type":"thinking","thinking":thinking,"signature":signature}));
        }
        let wire_content = wire
            .get_mut("content")
            .and_then(Value::as_array_mut)
            .context("Messages assistant content must be blocks")?;
        content.append(wire_content);
        *wire_content = content;
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/thinking_replay_tests.rs"]
mod tests;

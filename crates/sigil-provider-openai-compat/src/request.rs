use anyhow::Result;
use serde_json::{Value, json};

use sigil_kernel::{CompletionRequest, MessageRole, ModelMessage, ToolSpec};

use crate::models::{OpenAiChatCompletionRequest, OpenAiStreamOptions};

pub fn build_chat_request(request: &CompletionRequest) -> Result<OpenAiChatCompletionRequest> {
    let messages = request
        .messages
        .iter()
        .map(model_message_to_json)
        .collect::<Result<Vec<_>>>()?;
    let tools = openai_tools(&request.tools);

    Ok(OpenAiChatCompletionRequest {
        model: request.model_name.clone(),
        messages,
        stream: true,
        stream_options: Some(OpenAiStreamOptions {
            include_usage: true,
        }),
        tool_choice: tools.as_ref().map(|_| "auto".to_owned()),
        tools,
        temperature: request.temperature,
        max_tokens: request.max_tokens,
    })
}

fn model_message_to_json(message: &ModelMessage) -> Result<Value> {
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

    Ok(base)
}

fn openai_tools(tools: &[ToolSpec]) -> Option<Vec<Value>> {
    if tools.is_empty() {
        return None;
    }
    Some(
        tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema,
                    }
                })
            })
            .collect(),
    )
}

#[cfg(test)]
#[path = "tests/request_tests.rs"]
mod tests;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct AnthropicMessagesRequest {
    pub model: String,
    pub messages: Vec<Value>,
    pub max_tokens: u32,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicStreamEnvelope {
    MessageStart {
        message: AnthropicMessageStart,
    },
    ContentBlockStart {
        index: usize,
        content_block: AnthropicContentBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: AnthropicContentBlockDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: AnthropicMessageDelta,
        #[serde(default)]
        usage: Option<AnthropicUsage>,
    },
    MessageStop,
    Ping,
    Error {
        error: AnthropicErrorBody,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicMessageStart {
    #[serde(default)]
    pub usage: Option<AnthropicUsage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicContentBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicContentBlockDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        #[serde(default)]
        partial_json: String,
    },
    ThinkingDelta {
        #[serde(default)]
        thinking: String,
    },
    SignatureDelta {
        #[serde(default)]
        signature: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicMessageDelta {
    #[serde(default)]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AnthropicUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicErrorBody {
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub message: String,
}

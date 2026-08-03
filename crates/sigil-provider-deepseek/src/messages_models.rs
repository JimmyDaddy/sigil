use serde::{Deserialize, Serialize};
use serde_json::Value;

/// DeepSeek Anthropic-compatible Messages API request body (subset used by hosted
/// web-search turns). The wire shape mirrors the Anthropic Messages API.
#[derive(Clone, Serialize)]
pub struct DeepSeekMessagesRequest {
    pub model: String,
    pub messages: Vec<Value>,
    pub max_tokens: u32,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
}

/// SSE envelopes emitted by the DeepSeek Anthropic-compatible streaming endpoint.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeepSeekMessagesEnvelope {
    MessageStart {
        message: DeepSeekMessageStart,
    },
    ContentBlockStart {
        index: usize,
        content_block: DeepSeekContentBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: DeepSeekContentBlockDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: DeepSeekMessageDelta,
        #[serde(default)]
        usage: Option<DeepSeekUsage>,
    },
    MessageStop,
    Ping,
    Error {
        error: DeepSeekErrorBody,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeepSeekMessageStart {
    #[serde(default)]
    pub usage: Option<DeepSeekUsage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeepSeekContentBlock {
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
    ServerToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    WebSearchToolResult {
        tool_use_id: String,
        content: DeepSeekWebSearchToolResultContent,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        signature: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeepSeekContentBlockDelta {
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
    CitationsDelta {
        citation: DeepSeekCitation,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeepSeekMessageDelta {
    #[serde(default)]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DeepSeekUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub server_tool_use: Option<DeepSeekServerToolUsage>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DeepSeekServerToolUsage {
    #[serde(default)]
    pub web_search_requests: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum DeepSeekWebSearchToolResultContent {
    Results(Vec<DeepSeekWebSearchResult>),
    Error(DeepSeekWebSearchToolResultError),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeepSeekWebSearchResult {
    pub r#type: String,
    pub url: String,
    pub title: String,
    pub encrypted_content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_age: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeepSeekWebSearchToolResultError {
    pub r#type: String,
    pub error_code: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeepSeekCitation {
    WebSearchResultLocation {
        url: String,
        #[serde(default)]
        title: Option<String>,
        encrypted_index: String,
        cited_text: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeepSeekErrorBody {
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub message: String,
}

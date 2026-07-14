use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct DeepSeekChatCompletionRequest {
    pub model: String,
    pub messages: Vec<serde_json::Value>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<DeepSeekStreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

/// Stream controls required to receive terminal request usage from DeepSeek Chat Completions.
#[derive(Debug, Clone, Serialize)]
pub struct DeepSeekStreamOptions {
    pub include_usage: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeepSeekCompletionRequest {
    pub model: String,
    pub prompt: String,
    pub suffix: String,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeepSeekStreamEnvelope {
    #[serde(default)]
    pub choices: Vec<DeepSeekChoice>,
    #[serde(default)]
    pub usage: Option<DeepSeekUsage>,
    #[serde(default)]
    pub system_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeepSeekChoice {
    #[serde(default)]
    pub delta: DeepSeekDelta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DeepSeekDelta {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<DeepSeekToolCallDelta>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeepSeekToolCallDelta {
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<DeepSeekFunctionDelta>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeepSeekFunctionDelta {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeepSeekUsage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub prompt_cache_hit_tokens: Option<u64>,
    #[serde(default)]
    pub prompt_cache_miss_tokens: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeepSeekCompletionStreamEnvelope {
    #[serde(default)]
    pub choices: Vec<DeepSeekCompletionChoice>,
    #[serde(default)]
    pub usage: Option<DeepSeekUsage>,
    #[serde(default)]
    pub system_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeepSeekCompletionChoice {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

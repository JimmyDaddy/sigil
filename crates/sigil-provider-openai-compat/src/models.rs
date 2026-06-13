use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct OpenAiChatCompletionRequest {
    pub model: String,
    pub messages: Vec<serde_json::Value>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<OpenAiStreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAiStreamOptions {
    pub include_usage: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiStreamEnvelope {
    #[serde(default)]
    pub choices: Vec<OpenAiChoice>,
    #[serde(default)]
    pub usage: Option<OpenAiUsage>,
    #[serde(default)]
    pub system_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiChoice {
    #[serde(default)]
    pub delta: OpenAiDelta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct OpenAiDelta {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<OpenAiToolCallDelta>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiToolCallDelta {
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<OpenAiFunctionDelta>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiFunctionDelta {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiUsage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiPromptTokensDetails {
    #[serde(default)]
    pub cached_tokens: u64,
}

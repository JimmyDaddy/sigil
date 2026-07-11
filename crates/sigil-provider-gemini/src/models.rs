use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sigil_kernel::SecretString;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiGenerateContentRequest {
    pub contents: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GeminiGenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiStreamEnvelope {
    #[serde(default)]
    pub candidates: Vec<GeminiCandidate>,
    #[serde(default)]
    pub usage_metadata: Option<GeminiUsageMetadata>,
    #[serde(default)]
    pub prompt_feedback: Option<GeminiPromptFeedback>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiCandidate {
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub content: Option<GeminiContent>,
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub finish_message: Option<String>,
    #[serde(default)]
    pub safety_ratings: Vec<GeminiSafetyRating>,
    #[serde(default)]
    pub grounding_metadata: Option<GeminiGroundingMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeminiContent {
    #[serde(default)]
    pub parts: Vec<GeminiPart>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiPart {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub function_call: Option<GeminiFunctionCall>,
    #[serde(default)]
    pub thought_signature: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeminiFunctionCall {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GeminiUsageMetadata {
    #[serde(default)]
    pub prompt_token_count: u64,
    #[serde(default)]
    pub candidates_token_count: u64,
    #[serde(default)]
    pub cached_content_token_count: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiPromptFeedback {
    #[serde(default)]
    pub block_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GeminiSafetyRating {
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub probability: Option<String>,
    #[serde(default)]
    pub blocked: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GeminiGroundingMetadata {
    #[serde(default, deserialize_with = "deserialize_secret_strings")]
    pub web_search_queries: Vec<SecretString>,
    #[serde(default)]
    pub grounding_chunks: Vec<GeminiGroundingChunk>,
    #[serde(default)]
    pub grounding_supports: Vec<GeminiGroundingSupport>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct GeminiGroundingChunk {
    #[serde(default)]
    pub web: Option<GeminiWebGroundingChunk>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct GeminiWebGroundingChunk {
    #[serde(default, deserialize_with = "deserialize_optional_secret_string")]
    pub uri: Option<SecretString>,
    #[serde(default, deserialize_with = "deserialize_optional_secret_string")]
    pub title: Option<SecretString>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GeminiGroundingSupport {
    #[serde(default)]
    pub grounding_chunk_indices: Vec<usize>,
    #[serde(default)]
    pub segment: Option<GeminiGroundingSegment>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GeminiGroundingSegment {
    #[serde(default)]
    pub part_index: usize,
    #[serde(default)]
    pub start_index: Option<usize>,
    #[serde(default)]
    pub end_index: Option<usize>,
    #[serde(default, deserialize_with = "deserialize_optional_secret_string")]
    pub text: Option<SecretString>,
}

fn deserialize_secret_strings<'de, D>(deserializer: D) -> Result<Vec<SecretString>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<String>::deserialize(deserializer)
        .map(|values| values.into_iter().map(SecretString::new).collect())
}

fn deserialize_optional_secret_string<'de, D>(
    deserializer: D,
) -> Result<Option<SecretString>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(|value| value.map(SecretString::new))
}

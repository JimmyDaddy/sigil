use std::fmt;

use serde::Serialize;
use serde_json::{Value, value::RawValue};

#[derive(Clone, Serialize)]
pub struct OpenAiResponsesRequest {
    pub model: String,
    pub input: Vec<Value>,
    pub stream: bool,
    pub store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<OpenAiResponsesReasoning>,
}

impl fmt::Debug for OpenAiResponsesRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiResponsesRequest")
            .field("model", &self.model)
            .field("input_item_count", &self.input.len())
            .field("stream", &self.stream)
            .field("store", &self.store)
            .field("include", &self.include)
            .field("tool_count", &self.tools.as_ref().map(Vec::len))
            .field("tool_choice", &self.tool_choice)
            .field("temperature", &self.temperature)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("reasoning", &self.reasoning)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAiResponsesReasoning {
    pub effort: String,
}

/// The prompt-bearing subset accepted by the official Responses input-token endpoint.
///
/// `stream`, `store`, sampling controls and the output reservation do not alter input token
/// accounting. The provider validates the reservation separately before it turns the returned
/// count into a portable-compaction fit proof.
#[derive(Clone, Serialize)]
pub struct OpenAiResponsesInputTokenCountRequest {
    pub model: String,
    pub input: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<OpenAiResponsesReasoning>,
}

impl fmt::Debug for OpenAiResponsesInputTokenCountRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiResponsesInputTokenCountRequest")
            .field("model", &self.model)
            .field("input_item_count", &self.input.len())
            .field("tool_count", &self.tools.as_ref().map(Vec::len))
            .field("tool_choice", &self.tool_choice)
            .field("reasoning", &self.reasoning)
            .finish()
    }
}

/// Stateless request for the OpenAI Responses compaction endpoint.
///
/// `input` is the already-materialized canonical Responses window. It is intentionally not
/// rebuilt or normalized by this DTO.
#[derive(Clone, Serialize)]
pub struct OpenAiResponsesCompactRequest {
    pub model: String,
    pub input: Vec<Value>,
}

impl fmt::Debug for OpenAiResponsesCompactRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiResponsesCompactRequest")
            .field("model", &self.model)
            .field("input_item_count", &self.input.len())
            .finish()
    }
}

/// Opaque canonical window returned by `/responses/compact`.
///
/// The provider validates only the response envelope. Individual output items are deliberately
/// not decoded, filtered, or rewritten because OpenAI requires the returned window to be used
/// unchanged in a subsequent `/responses` request.
#[derive(Debug, Clone)]
pub struct OpenAiResponsesCompactedWindow {
    pub response_id: String,
    pub(crate) output: Box<RawValue>,
}

impl OpenAiResponsesCompactedWindow {
    /// Returns the complete response `output` array exactly as received from the compaction
    /// endpoint. Callers must treat it as provider-opaque and pass/store it unchanged.
    #[must_use]
    pub fn canonical_output_json(&self) -> &str {
        self.output.get()
    }
}

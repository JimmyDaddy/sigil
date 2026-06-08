use std::{collections::BTreeMap, pin::Pin};

use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Provider-agnostic request materialization sent to a model backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CompletionRequest {
    pub provider_name: String,
    pub model_name: String,
    pub messages: Vec<ModelMessage>,
    pub tools: Vec<crate::tool::ToolSpec>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub previous_response_handle: Option<ResponseHandle>,
    #[serde(default)]
    pub continuation_states: Vec<ProviderContinuationState>,
    pub traffic_partition_key: Option<String>,
    pub background: bool,
    pub store: bool,
    pub deterministic_materialization: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    Max,
}

impl ReasoningEffort {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
        }
    }
}

/// Capability flags exposed by a provider implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProviderCapabilities {
    pub exact_prefix_cache: bool,
    pub reports_cache_tokens: bool,
    pub supports_reasoning_stream: bool,
    pub supports_tool_stream: bool,
    pub supports_background_tasks: bool,
    pub supports_response_handles: bool,
    pub supports_reasoning_artifacts: bool,
    pub supports_structured_output: bool,
    pub supports_assistant_prefix_seed: bool,
    pub supports_schema_constrained_tools: bool,
    pub supports_infill_completion: bool,
    pub supports_system_fingerprint: bool,
}

/// Incremental stream events emitted by a provider while serving a request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProviderChunk {
    TextDelta(String),
    ReasoningDelta(String),
    ReasoningSummaryDelta(String),
    ToolCallStart { id: String, name: String },
    ToolCallArgsDelta { id: String, delta: String },
    ToolCallComplete(ToolCall),
    Usage(UsageStats),
    BackgroundTaskAccepted(BackgroundTaskHandle),
    BackgroundTaskStatus(BackgroundTaskStatus),
    ResponseHandle(ResponseHandle),
    ReasoningArtifact(ReasoningArtifact),
    ContinuationState(ProviderContinuationState),
    Done,
}

/// Structured tool call produced by a model provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args_json: String,
}

/// Provider-facing chat message persisted in session history.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ModelMessage {
    pub id: String,
    pub role: MessageRole,
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// Usage accounting emitted by a provider for a single request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UsageStats {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_hit_tokens: u64,
    pub cache_miss_tokens: u64,
    pub input_cost: f64,
    pub output_cost: f64,
    pub cache_savings: f64,
    pub system_fingerprint: Option<String>,
}

impl Default for UsageStats {
    fn default() -> Self {
        Self {
            prompt_tokens: 0,
            completion_tokens: 0,
            cache_hit_tokens: 0,
            cache_miss_tokens: 0,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        }
    }
}

/// Stable snapshot of the deterministic prefix materialization for auditing and resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PrefixSnapshot {
    pub materialized_text: String,
    pub sha256: String,
    pub provider_name: String,
    pub model_name: String,
    pub memory_fingerprint: String,
    pub tool_schema_fingerprint: String,
    pub skill_index_fingerprint: String,
}

/// Provider-specific response handle that can be reused across turns or resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ResponseHandle {
    pub provider_name: String,
    pub response_id: String,
    pub continuation_cursor: Option<String>,
}

/// Durable handle for provider-managed background work.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BackgroundTaskHandle {
    pub provider_name: String,
    pub task_id: String,
    pub resumable: bool,
}

/// Latest known status for a provider-managed background task.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BackgroundTaskStatus {
    pub provider_name: String,
    pub task_id: String,
    pub status: String,
    pub metadata: BTreeMap<String, Value>,
}

/// Provider-specific reasoning artifact that should not be interpreted by the kernel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ReasoningArtifact {
    pub provider_name: String,
    pub opaque_blob: Value,
}

/// Opaque continuation state that must survive turn boundaries and process restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProviderContinuationState {
    pub provider_name: String,
    pub state_kind: String,
    pub message_id: Option<String>,
    pub opaque_blob: Value,
}

/// Aggregated usage counters across the lifetime of a session.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct SessionStats {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_hit_tokens: u64,
    pub cache_miss_tokens: u64,
    pub input_cost: f64,
    pub output_cost: f64,
    pub cache_savings: f64,
    #[serde(default)]
    pub last_prompt_tokens: u64,
}

impl SessionStats {
    /// Merges one request's usage counters into the running session totals.
    pub fn apply_usage(&mut self, usage: &UsageStats) {
        self.prompt_tokens += usage.prompt_tokens;
        self.completion_tokens += usage.completion_tokens;
        self.cache_hit_tokens += usage.cache_hit_tokens;
        self.cache_miss_tokens += usage.cache_miss_tokens;
        self.input_cost += usage.input_cost;
        self.output_cost += usage.output_cost;
        self.cache_savings += usage.cache_savings;
        self.last_prompt_tokens = usage.prompt_tokens;
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// Returns the stable provider registry name.
    fn name(&self) -> &str;

    /// Returns the provider's declared runtime capabilities.
    fn capabilities(&self) -> ProviderCapabilities;

    /// Starts a streaming completion request.
    ///
    /// # Errors
    ///
    /// Returns an error when request materialization, transport setup, authentication,
    /// or provider-side execution fails before a usable stream can be established.
    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>>;
}

impl ModelMessage {
    /// Creates a system-role message.
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(MessageRole::System, Some(content.into()))
    }

    /// Creates a user-role message.
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(MessageRole::User, Some(content.into()))
    }

    /// Creates an assistant-role message with optional structured tool calls.
    pub fn assistant(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        let mut message = Self::new(MessageRole::Assistant, content);
        message.tool_calls = tool_calls;
        message
    }

    /// Creates a tool-role message bound to a prior tool call id.
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        let mut message = Self::new(MessageRole::Tool, Some(content.into()));
        message.tool_call_id = Some(tool_call_id.into());
        message
    }

    /// Creates a message with a fresh opaque identifier.
    pub fn new(role: MessageRole, content: Option<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            role,
            content,
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SessionStats, UsageStats};

    #[test]
    fn session_stats_track_latest_prompt_tokens_separately_from_totals() {
        let mut stats = SessionStats::default();
        stats.apply_usage(&UsageStats {
            prompt_tokens: 120,
            completion_tokens: 10,
            cache_hit_tokens: 80,
            cache_miss_tokens: 40,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        });
        stats.apply_usage(&UsageStats {
            prompt_tokens: 42,
            completion_tokens: 5,
            cache_hit_tokens: 21,
            cache_miss_tokens: 21,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        });

        assert_eq!(stats.prompt_tokens, 162);
        assert_eq!(stats.last_prompt_tokens, 42);
    }
}

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    pin::Pin,
};

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosted_tools: Vec<crate::HostedToolRequest>,
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

/// Declares how a provider can surface model reasoning deltas.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningStreamSupport {
    #[default]
    Unsupported,
    Passthrough,
    Native,
}

impl ReasoningStreamSupport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Passthrough => "passthrough",
            Self::Native => "native",
        }
    }

    pub fn can_surface(self) -> bool {
        !matches!(self, Self::Unsupported)
    }
}

/// Capability flags exposed by a provider implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProviderCapabilities {
    pub exact_prefix_cache: bool,
    pub reports_cache_tokens: bool,
    #[serde(default)]
    pub reasoning_stream: ReasoningStreamSupport,
    #[serde(default)]
    pub supports_reasoning_effort: bool,
    pub supports_tool_stream: bool,
    pub supports_background_tasks: bool,
    pub supports_response_handles: bool,
    pub supports_reasoning_artifacts: bool,
    pub supports_structured_output: bool,
    pub supports_assistant_prefix_seed: bool,
    pub supports_schema_constrained_tools: bool,
    #[serde(default)]
    pub supports_agent_background_resume: bool,
    #[serde(default)]
    pub supports_agent_thread_usage: bool,
    #[serde(default)]
    pub supports_agent_result_replay: bool,
    pub supports_infill_completion: bool,
    pub supports_system_fingerprint: bool,
    pub tool_name_max_chars: usize,
}

impl ProviderCapabilities {
    pub fn can_surface_reasoning_stream(&self) -> bool {
        self.reasoning_stream.can_surface()
    }
}

/// Incremental stream events emitted by a provider while serving a request.
#[derive(Clone)]
pub enum ProviderChunk {
    TextDelta(String),
    ReasoningDelta(String),
    ReasoningSummaryDelta(String),
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallArgsDelta {
        id: String,
        delta: String,
    },
    ToolCallComplete(ToolCall),
    Usage(UsageStats),
    BackgroundTaskAccepted(BackgroundTaskHandle),
    BackgroundTaskStatus(BackgroundTaskStatus),
    ResponseHandle(ResponseHandle),
    ReasoningArtifact(ReasoningArtifact),
    ContinuationState(ProviderContinuationState),
    ToolCallStreamError(crate::SafePersistenceError),
    HostedToolStarted {
        authorization_id: String,
        invocation_id: String,
        kind: crate::HostedToolKind,
    },
    HostedEvidence {
        authorization_id: String,
        invocation_id: String,
        kind: crate::HostedToolKind,
        evidence: crate::HostedEvidence,
    },
    HostedToolFailed {
        authorization_id: String,
        invocation_id: String,
        kind: crate::HostedToolKind,
        failure_class: crate::WebSearchFailureClass,
    },
    HostedRequestUsage {
        authorization_id: String,
        kind: crate::HostedToolKind,
        observed_uses: u32,
    },
    Done,
}

impl fmt::Debug for ProviderChunk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TextDelta(value) => formatter
                .debug_tuple("TextDelta")
                .field(&format_args!("[redacted; {} bytes]", value.len()))
                .finish(),
            Self::ReasoningDelta(value) => formatter
                .debug_tuple("ReasoningDelta")
                .field(&format_args!("[redacted; {} bytes]", value.len()))
                .finish(),
            Self::ReasoningSummaryDelta(value) => formatter
                .debug_tuple("ReasoningSummaryDelta")
                .field(&format_args!("[redacted; {} bytes]", value.len()))
                .finish(),
            Self::ToolCallStart { id, name } => formatter
                .debug_struct("ToolCallStart")
                .field("id", &format_args!("[redacted; {} bytes]", id.len()))
                .field("name", &format_args!("[redacted; {} bytes]", name.len()))
                .finish(),
            Self::ToolCallArgsDelta { id, delta } => formatter
                .debug_struct("ToolCallArgsDelta")
                .field("id", &format_args!("[redacted; {} bytes]", id.len()))
                .field("delta", &format_args!("[redacted; {} bytes]", delta.len()))
                .finish(),
            Self::ToolCallComplete(call) => formatter
                .debug_struct("ToolCallComplete")
                .field("id", &format_args!("[redacted; {} bytes]", call.id.len()))
                .field(
                    "name",
                    &format_args!("[redacted; {} bytes]", call.name.len()),
                )
                .field(
                    "args_json",
                    &format_args!("[redacted; {} bytes]", call.args_json.len()),
                )
                .finish(),
            Self::Usage(value) => formatter.debug_tuple("Usage").field(value).finish(),
            Self::BackgroundTaskAccepted(value) => formatter
                .debug_tuple("BackgroundTaskAccepted")
                .field(value)
                .finish(),
            Self::BackgroundTaskStatus(value) => formatter
                .debug_tuple("BackgroundTaskStatus")
                .field(value)
                .finish(),
            Self::ResponseHandle(value) => formatter
                .debug_tuple("ResponseHandle")
                .field(value)
                .finish(),
            Self::ReasoningArtifact(_) => formatter.write_str("ReasoningArtifact([redacted])"),
            Self::ContinuationState(_) => formatter.write_str("ContinuationState([redacted])"),
            Self::ToolCallStreamError(error) => formatter
                .debug_tuple("ToolCallStreamError")
                .field(error)
                .finish(),
            Self::HostedToolStarted {
                authorization_id,
                invocation_id,
                kind,
            } => formatter
                .debug_struct("HostedToolStarted")
                .field(
                    "authorization_id",
                    &format_args!("[safe-id; {} bytes]", authorization_id.len()),
                )
                .field(
                    "invocation_id",
                    &format_args!("[safe-id; {} bytes]", invocation_id.len()),
                )
                .field("kind", kind)
                .finish(),
            Self::HostedEvidence {
                authorization_id,
                invocation_id,
                kind,
                evidence,
            } => formatter
                .debug_struct("HostedEvidence")
                .field(
                    "authorization_id",
                    &format_args!("[safe-id; {} bytes]", authorization_id.len()),
                )
                .field(
                    "invocation_id",
                    &format_args!("[safe-id; {} bytes]", invocation_id.len()),
                )
                .field("kind", kind)
                .field("evidence", evidence)
                .finish(),
            Self::HostedToolFailed {
                authorization_id,
                invocation_id,
                kind,
                failure_class,
            } => formatter
                .debug_struct("HostedToolFailed")
                .field(
                    "authorization_id",
                    &format_args!("[safe-id; {} bytes]", authorization_id.len()),
                )
                .field(
                    "invocation_id",
                    &format_args!("[safe-id; {} bytes]", invocation_id.len()),
                )
                .field("kind", kind)
                .field("failure_class", failure_class)
                .finish(),
            Self::HostedRequestUsage {
                authorization_id,
                kind,
                observed_uses,
            } => formatter
                .debug_struct("HostedRequestUsage")
                .field(
                    "authorization_id",
                    &format_args!("[safe-id; {} bytes]", authorization_id.len()),
                )
                .field("kind", kind)
                .field("observed_uses", observed_uses)
                .finish(),
            Self::Done => formatter.write_str("Done"),
        }
    }
}

/// Structured tool call produced by a model provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args_json: String,
}

/// Controls whether a completed streamed tool call may use a generated fallback id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallCompletionIdPolicy {
    /// Complete only when the provider emitted an explicit tool-call id.
    RequireProviderId,
    /// Complete with `call-{index}` when the provider omitted an id.
    SynthesizeFromIndex,
}

/// Provider-neutral accumulator for streamed tool-call deltas.
#[derive(Clone, Default)]
pub struct ToolCallStreamAccumulator {
    parts: BTreeMap<usize, ToolCallStreamPart>,
    total_args_bytes: usize,
    completed_indices: BTreeSet<usize>,
    completed_call_ids: BTreeSet<String>,
    terminal_error: Option<crate::SafePersistenceError>,
}

#[derive(Clone, Default)]
struct ToolCallStreamPart {
    id: Option<String>,
    event_id: Option<String>,
    name: Option<String>,
    args: String,
    started: bool,
}

impl fmt::Debug for ToolCallStreamAccumulator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolCallStreamAccumulator")
            .field("open_call_count", &self.parts.len())
            .field("total_args_bytes", &self.total_args_bytes)
            .field("completed_index_count", &self.completed_indices.len())
            .field("completed_call_id_count", &self.completed_call_ids.len())
            .field("terminal_error", &self.terminal_error.is_some())
            .finish()
    }
}

impl ToolCallStreamAccumulator {
    /// Creates an empty accumulator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one provider tool-call delta and appends any emitted stream chunks.
    pub fn append_delta(
        &mut self,
        chunks: &mut Vec<ProviderChunk>,
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    ) {
        if self.terminal_error.is_some() {
            return;
        }
        if self.completed_indices.contains(&index) {
            self.fail(
                chunks,
                crate::SafePersistenceError::ToolCallStreamInvalid {
                    reason: "provider emitted a delta after completing the same tool-call index"
                        .to_owned(),
                },
            );
            return;
        }
        if !self.parts.contains_key(&index)
            && self
                .parts
                .len()
                .saturating_add(self.completed_indices.len())
                >= crate::MAX_PROVIDER_TURN_TOOL_CALLS
        {
            self.fail(
                chunks,
                crate::SafePersistenceError::ToolCallStreamInvalid {
                    reason: format!(
                        "provider turn exceeded {} streamed tool calls",
                        crate::MAX_PROVIDER_TURN_TOOL_CALLS
                    ),
                },
            );
            return;
        }
        if let Some(id) = id.as_deref()
            && let Err(error) = crate::persistence::validate_tool_call_id(id)
        {
            self.fail(chunks, error);
            return;
        }
        if let Some(name) = name.as_deref()
            && let Err(error) = crate::persistence::validate_tool_call_name(name)
        {
            self.fail(chunks, error);
            return;
        }
        let argument_bytes = arguments.as_ref().map_or(0, String::len);
        let existing_args_bytes = self.parts.get(&index).map_or(0, |part| part.args.len());
        let next_call_bytes = existing_args_bytes.saturating_add(argument_bytes);
        let next_total_bytes = self.total_args_bytes.saturating_add(argument_bytes);
        if next_call_bytes > crate::MAX_STREAMED_TOOL_ARGS_BYTES {
            self.fail(
                chunks,
                crate::SafePersistenceError::ToolArgsTooLarge {
                    observed_bytes: next_call_bytes,
                    limit_bytes: crate::MAX_STREAMED_TOOL_ARGS_BYTES,
                },
            );
            return;
        }
        if next_total_bytes > crate::MAX_PROVIDER_TURN_TOOL_ARGS_BYTES {
            self.fail(
                chunks,
                crate::SafePersistenceError::ToolArgsTooLarge {
                    observed_bytes: next_total_bytes,
                    limit_bytes: crate::MAX_PROVIDER_TURN_TOOL_ARGS_BYTES,
                },
            );
            return;
        }
        if let Some(existing) = self.parts.get(&index) {
            if let Some(id) = id.as_ref()
                && existing.id.as_ref().is_some_and(|current| current != id)
            {
                self.fail(
                    chunks,
                    crate::SafePersistenceError::ToolCallStreamInvalid {
                        reason: "provider changed a streamed tool-call id".to_owned(),
                    },
                );
                return;
            }
            if let Some(name) = name.as_ref()
                && existing
                    .name
                    .as_ref()
                    .is_some_and(|current| current != name)
            {
                self.fail(
                    chunks,
                    crate::SafePersistenceError::ToolCallStreamInvalid {
                        reason: "provider changed a streamed tool-call name".to_owned(),
                    },
                );
                return;
            }
        }
        let part = self.parts.entry(index).or_default();
        if let Some(id) = id {
            part.id = Some(id);
        }
        if let Some(name) = name {
            part.name = Some(name);
            emit_tool_start(chunks, index, part);
        }
        if let Some(arguments) = arguments {
            part.args.push_str(&arguments);
            self.total_args_bytes = next_total_bytes;
            chunks.push(ProviderChunk::ToolCallArgsDelta {
                id: stable_tool_call_id(index, part),
                delta: arguments,
            });
        }
    }

    /// Completes every currently open tool call that has enough provider data.
    pub fn complete_open_calls(
        &mut self,
        chunks: &mut Vec<ProviderChunk>,
        id_policy: ToolCallCompletionIdPolicy,
    ) {
        if self.terminal_error.is_some() {
            return;
        }
        let mut completed = Vec::new();
        let mut pending_call_ids = BTreeSet::new();
        let mut pending_error = None;
        for (index, part) in &mut self.parts {
            emit_tool_start(chunks, *index, part);
            let Some(name) = part.name.clone() else {
                continue;
            };
            let Some(id) = completion_tool_call_id(*index, part, id_policy) else {
                continue;
            };
            if self.completed_call_ids.contains(&id) || !pending_call_ids.insert(id.clone()) {
                pending_error = Some(crate::SafePersistenceError::ToolCallStreamInvalid {
                    reason: "provider reused a completed tool-call id".to_owned(),
                });
                break;
            }
            completed.push((
                *index,
                ToolCall {
                    id: id.clone(),
                    name,
                    args_json: part.args.clone(),
                },
                part.args.len(),
            ));
        }
        if let Some(error) = pending_error {
            self.fail(chunks, error);
            return;
        }
        for (index, call, _args_bytes) in completed {
            let id = call.id.clone();
            chunks.push(ProviderChunk::ToolCallComplete(call));
            self.completed_indices.insert(index);
            self.completed_call_ids.insert(id);
            self.parts.remove(&index);
        }
    }

    /// Discards all buffered streamed tool-call state.
    pub fn clear(&mut self) {
        self.parts.clear();
        self.total_args_bytes = 0;
        self.completed_indices.clear();
        self.completed_call_ids.clear();
        self.terminal_error = None;
    }

    fn fail(&mut self, chunks: &mut Vec<ProviderChunk>, error: crate::SafePersistenceError) {
        self.parts.clear();
        self.total_args_bytes = 0;
        self.terminal_error = Some(error.clone());
        chunks.push(ProviderChunk::ToolCallStreamError(error));
    }
}

fn emit_tool_start(chunks: &mut Vec<ProviderChunk>, index: usize, part: &mut ToolCallStreamPart) {
    if part.started {
        return;
    }
    let Some(name) = part.name.clone() else {
        return;
    };
    chunks.push(ProviderChunk::ToolCallStart {
        id: stable_tool_call_id(index, part),
        name,
    });
    part.started = true;
}

fn completion_tool_call_id(
    index: usize,
    part: &mut ToolCallStreamPart,
    policy: ToolCallCompletionIdPolicy,
) -> Option<String> {
    match (part.id.is_some(), policy) {
        (true, _) => Some(stable_tool_call_id(index, part)),
        (false, ToolCallCompletionIdPolicy::SynthesizeFromIndex) => {
            Some(stable_tool_call_id(index, part))
        }
        (false, ToolCallCompletionIdPolicy::RequireProviderId) => None,
    }
}

fn stable_tool_call_id(index: usize, part: &mut ToolCallStreamPart) -> String {
    if let Some(id) = part.event_id.clone() {
        return id;
    }
    let id = part.id.clone().unwrap_or_else(|| format!("call-{index}"));
    part.event_id = Some(id.clone());
    id
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_kind: Option<AssistantMessageKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_attachments: Vec<crate::ImageAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// UI-facing phase for assistant messages recorded in the durable session log.
///
/// Provider request mappers ignore this field; it exists to keep transcript rendering and
/// restore behavior from treating tool preambles as final user-visible replies.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssistantMessageKind {
    ToolPreamble,
    Progress,
    ReasoningTrace,
    FinalAnswer,
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

    /// Returns native hosted web-search support for one exact model id.
    fn hosted_web_search_capability(&self, _model_name: &str) -> crate::HostedWebSearchCapability {
        crate::HostedWebSearchCapability::default()
    }

    /// Returns exact-model image input support. Compatible or unknown providers fail closed by
    /// default instead of inferring multimodal support from a text protocol shape.
    fn image_input_capability(&self, _model_name: &str) -> crate::ImageInputCapability {
        crate::ImageInputCapability::Unsupported
    }

    /// Classifies a provider-declared request rejection that is proven to have happened before
    /// any model generation or side effect.
    ///
    /// Providers must return `None` for generic HTTP statuses, free-form error messages, and
    /// compatible endpoints. A non-`None` value permits later recovery logic to reason about the
    /// durable physical-attempt terminal without parsing an error string.
    fn classify_pre_generation_rejection(
        &self,
        _error: &anyhow::Error,
    ) -> Option<ProviderRequestRejection> {
        None
    }

    /// Proves that one frozen portable-compaction target fits the provider/model request budget.
    ///
    /// Implementations may use a provider-owned exact measurement endpoint, but must return an
    /// error unless the resulting material is bound to the supplied frozen request, an explicit
    /// versioned profile, and a complete output/safety budget. Callers must record a durable
    /// non-generating physical-attempt lifecycle before invoking a remote implementation and may
    /// use it only after a durable pre-generation context-window rejection.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider has no exact target-proof capability, the frozen
    /// request is outside an admitted profile, or exact measurement cannot be established.
    async fn prove_portable_compaction_target(
        &self,
        _frozen_request: crate::FrozenProviderRequestMaterial,
    ) -> Result<crate::PortableTargetRequestMaterial> {
        anyhow::bail!("provider does not support exact portable-compaction target proof")
    }

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

#[async_trait]
impl<P> Provider for Box<P>
where
    P: Provider + ?Sized,
{
    fn name(&self) -> &str {
        (**self).name()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        (**self).capabilities()
    }

    fn hosted_web_search_capability(&self, model_name: &str) -> crate::HostedWebSearchCapability {
        (**self).hosted_web_search_capability(model_name)
    }

    fn image_input_capability(&self, model_name: &str) -> crate::ImageInputCapability {
        (**self).image_input_capability(model_name)
    }

    fn classify_pre_generation_rejection(
        &self,
        error: &anyhow::Error,
    ) -> Option<ProviderRequestRejection> {
        (**self).classify_pre_generation_rejection(error)
    }

    async fn prove_portable_compaction_target(
        &self,
        frozen_request: crate::FrozenProviderRequestMaterial,
    ) -> Result<crate::PortableTargetRequestMaterial> {
        (**self)
            .prove_portable_compaction_target(frozen_request)
            .await
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        (**self).stream(request).await
    }
}

/// A provider-specific rejection fact expressed without leaking provider error types into the
/// kernel. Every variant denotes a request the provider proved was rejected before generation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRequestRejection {
    ContextWindowExceeded,
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

    /// Creates an assistant-role message with an explicit transcript phase.
    pub fn assistant_with_kind(
        content: Option<String>,
        tool_calls: Vec<ToolCall>,
        assistant_kind: AssistantMessageKind,
    ) -> Self {
        let mut message = Self::assistant(content, tool_calls);
        message.assistant_kind = Some(assistant_kind);
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
            assistant_kind: None,
            image_attachments: Vec::new(),
        }
    }
}

#[cfg(test)]
#[path = "tests/provider_tests.rs"]
mod tests;

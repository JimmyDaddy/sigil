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

/// Provider-neutral cache behavior exposed by one exact configured route.
///
/// Adapter implementations must return [`Self::Unknown`] for unverified compatible endpoints.
/// In particular, a model name alone is never sufficient evidence for a vendor cache contract.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CacheMode {
    #[default]
    Unknown,
    /// The adapter can observe cache usage but has no conformance evidence for how entries are
    /// created or addressed.
    ObservedImplicitOrNone,
    /// Exact serialized prefixes are cached without client-authored breakpoints.
    ImplicitPrefix,
    /// The wire protocol accepts explicit cache breakpoints.
    ExplicitBreakpoints,
    /// Prefix caching is implicit, while the client owns stable logical routing boundaries.
    ImplicitPrefixWithLogicalBreakpoints,
}

impl CacheMode {
    fn supports_explicit_breakpoint_limit(self) -> bool {
        matches!(
            self,
            Self::ExplicitBreakpoints | Self::ImplicitPrefixWithLogicalBreakpoints
        )
    }
}

/// One provider-selectable cache lifetime.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CacheTtl {
    pub seconds: u32,
    /// Whether omitting a wire-level TTL selects this lifetime.
    pub is_default: bool,
}

/// Cache counters that a configured adapter can map into [`CacheUsageV1`].
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CacheUsageCapabilities {
    pub read_tokens: bool,
    pub write_tokens: bool,
    pub miss_tokens: bool,
}

/// Provider-neutral constraints on a server-retained conversation handle.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct StatefulContinuationCapability {
    /// Whether using the handle requires the connection to permit provider-side retention.
    pub requires_provider_retention: bool,
    /// Whether Sigil can fall back to a fully portable, stateless request.
    pub supports_stateless_fallback: bool,
}

/// Provider-neutral constraints on an adapter-owned native compaction operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct NativeCompactionCapability {
    /// Whether activation is restricted to an exact connection/model/protocol binding.
    pub requires_exact_route_binding: bool,
    /// Whether every native result is paired with a portable recovery checkpoint.
    pub supports_portable_fallback: bool,
}

/// How far an opaque native carrier may move without revalidation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeCarrierPortability {
    #[default]
    Unavailable,
    ConnectionModelProtocolBound,
    RouteFamilyBound,
}

/// Cache and continuation contract for one exact configured provider route and model.
///
/// This contract deliberately contains no vendor field names. Provider adapters retain
/// ownership of wire-specific keys, beta headers and opaque carrier formats.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderContextCapabilities {
    pub cache_mode: CacheMode,
    pub explicit_breakpoint_limit: Option<u8>,
    pub cache_ttls: Vec<CacheTtl>,
    pub cache_usage_fields: CacheUsageCapabilities,
    pub stateful_continuation: Option<StatefulContinuationCapability>,
    pub native_compaction: Option<NativeCompactionCapability>,
    pub native_carrier_portability: NativeCarrierPortability,
}

impl ProviderContextCapabilities {
    /// Conservative contract for an unknown or unverified route.
    #[must_use]
    pub fn unknown() -> Self {
        Self::default()
    }

    /// Conservative compatible-route contract that permits telemetry only after observation.
    #[must_use]
    pub fn observed_implicit_or_none(cache_usage_fields: CacheUsageCapabilities) -> Self {
        Self {
            cache_mode: CacheMode::ObservedImplicitOrNone,
            cache_usage_fields,
            ..Self::default()
        }
    }

    /// Validates cross-field invariants before a capability is used for request shaping.
    ///
    /// # Errors
    ///
    /// Returns an error for impossible breakpoint, TTL, or native-carrier combinations.
    pub fn validate(&self) -> Result<()> {
        if self.explicit_breakpoint_limit.is_some()
            != self.cache_mode.supports_explicit_breakpoint_limit()
        {
            anyhow::bail!(
                "cache breakpoint limit must be present exactly for an explicit or logical-breakpoint mode"
            );
        }
        if self.explicit_breakpoint_limit == Some(0) {
            anyhow::bail!("cache breakpoint limit must be positive");
        }
        let mut ttl_seconds = BTreeSet::new();
        let mut default_ttl_count = 0_u8;
        for ttl in &self.cache_ttls {
            if ttl.seconds == 0 {
                anyhow::bail!("cache TTL must be positive");
            }
            if !ttl_seconds.insert(ttl.seconds) {
                anyhow::bail!("cache TTL entries must be unique");
            }
            default_ttl_count = default_ttl_count.saturating_add(u8::from(ttl.is_default));
        }
        if default_ttl_count > 1 {
            anyhow::bail!("at most one cache TTL may be the default");
        }
        if self.native_compaction.is_some()
            == matches!(
                self.native_carrier_portability,
                NativeCarrierPortability::Unavailable
            )
        {
            anyhow::bail!(
                "native compaction and native carrier portability must be declared together"
            );
        }
        Ok(())
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_usage: Option<CacheUsageV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_snapshot: Option<ModelPricingSnapshotV1>,
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
            cache_usage: None,
            pricing_snapshot: None,
        }
    }
}

/// Origin of one normalized provider cache token count.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CacheTokenCountProvenance {
    /// The provider response exposed this exact cache category.
    ProviderReported,
    /// Sigil derived this value from a provider-reported total and another reported category.
    DerivedFromProviderTotal,
}

/// One cache token count plus evidence describing how it was obtained.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CacheTokenCountV1 {
    pub tokens: u64,
    pub provenance: CacheTokenCountProvenance,
}

impl CacheTokenCountV1 {
    #[must_use]
    pub fn provider_reported(tokens: u64) -> Self {
        Self {
            tokens,
            provenance: CacheTokenCountProvenance::ProviderReported,
        }
    }

    #[must_use]
    pub fn derived_from_provider_total(tokens: u64) -> Self {
        Self {
            tokens,
            provenance: CacheTokenCountProvenance::DerivedFromProviderTotal,
        }
    }
}

/// Provider-neutral cache read/write/uncached accounting for one request.
///
/// Missing fields are unknown, not zero. Legacy `cache_hit_tokens` / `cache_miss_tokens` remain
/// available during migration, while this shape preserves the provider-report provenance needed
/// by economics admission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CacheUsageV1 {
    pub schema_version: u16,
    pub read: Option<CacheTokenCountV1>,
    pub write: Option<CacheTokenCountV1>,
    pub uncached: Option<CacheTokenCountV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_layout_mutation: Option<crate::CacheLayoutMutationKind>,
    /// Provider-reported uncached input was observed even though the immediately preceding local
    /// request layout was byte-identical. This is deliberately narrower than a TTL or eviction
    /// diagnosis: it proves only that Sigil did not mutate the compared logical request.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provider_miss_without_local_mutation: bool,
}

impl CacheUsageV1 {
    pub const SCHEMA_VERSION: u16 = 1;

    #[must_use]
    pub fn reported_read_with_derived_uncached(total_input: u64, read_tokens: u64) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            read: Some(CacheTokenCountV1::provider_reported(read_tokens)),
            write: None,
            uncached: Some(CacheTokenCountV1::derived_from_provider_total(
                total_input.saturating_sub(read_tokens),
            )),
            local_layout_mutation: None,
            provider_miss_without_local_mutation: false,
        }
    }

    /// Attaches the local request-layout evidence and derives the narrow provider-miss diagnostic.
    ///
    /// A positive value never claims why the provider missed; TTL expiry, eviction and remote
    /// routing remain unproven.
    pub fn observe_local_layout(
        &mut self,
        mutation: crate::CacheLayoutMutationKind,
        legacy_cache_miss_tokens: u64,
    ) {
        self.local_layout_mutation = Some(mutation);
        let uncached_tokens = self
            .uncached
            .as_ref()
            .map_or(legacy_cache_miss_tokens, |count| count.tokens);
        self.provider_miss_without_local_mutation =
            mutation == crate::CacheLayoutMutationKind::Identical && uncached_tokens > 0;
    }

    /// Validates schema and rejects internally inconsistent cache totals.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported versions or when the sum of all known cache categories
    /// exceeds the provider-reported input total.
    pub fn validate_for_prompt_tokens(&self, prompt_tokens: u64) -> Result<()> {
        if self.schema_version != Self::SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported cache usage schema version {}",
                self.schema_version
            );
        }
        let known_cache_tokens = [&self.read, &self.write, &self.uncached]
            .into_iter()
            .flatten()
            .fold(0_u64, |total, count| total.saturating_add(count.tokens));
        if known_cache_tokens > prompt_tokens {
            anyhow::bail!("known cache token categories exceed provider prompt tokens");
        }
        Ok(())
    }
}

/// Trusted, versioned price evidence used to explain one request's effective cost.
///
/// Prices are USD per `unit_tokens`. A missing cache-write price means the route does not expose a
/// separately billable write category in this snapshot; it must not be interpreted as zero when a
/// provider reports write tokens.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ModelPricingSnapshotV1 {
    pub schema_version: u16,
    pub snapshot_id: String,
    pub currency: String,
    pub unit_tokens: u64,
    pub cache_read_per_unit: f64,
    pub cache_write_per_unit: Option<f64>,
    pub uncached_input_per_unit: f64,
    pub output_per_unit: f64,
    pub source: String,
    pub verified_at: String,
}

impl ModelPricingSnapshotV1 {
    pub const SCHEMA_VERSION: u16 = 1;

    /// Validates trusted price evidence before it is attached to durable usage.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported schema, incomplete identity or non-finite/negative prices.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != Self::SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported model pricing snapshot schema version {}",
                self.schema_version
            );
        }
        if self.snapshot_id.trim().is_empty()
            || self.currency != "USD"
            || self.unit_tokens == 0
            || self.source.trim().is_empty()
            || self.verified_at.trim().is_empty()
        {
            anyhow::bail!("model pricing snapshot identity is incomplete");
        }
        for (label, value) in [
            ("cache read", Some(self.cache_read_per_unit)),
            ("cache write", self.cache_write_per_unit),
            ("uncached input", Some(self.uncached_input_per_unit)),
            ("output", Some(self.output_per_unit)),
        ] {
            if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
                anyhow::bail!("model pricing snapshot {label} price is invalid");
            }
        }
        Ok(())
    }

    /// Attaches this snapshot and calculates request costs when all billable categories are known.
    ///
    /// A reported cache-write count without a write price leaves existing cost fields unchanged,
    /// rather than silently treating the write as free.
    ///
    /// # Errors
    ///
    /// Returns an error when the price or cache usage evidence is malformed.
    pub fn apply_to_usage(&self, mut usage: UsageStats) -> Result<UsageStats> {
        self.validate()?;
        let Some(cache_usage) = usage.cache_usage.as_ref() else {
            usage.pricing_snapshot = Some(self.clone());
            return Ok(usage);
        };
        cache_usage.validate_for_prompt_tokens(usage.prompt_tokens)?;
        let (Some(read), Some(uncached)) = (&cache_usage.read, &cache_usage.uncached) else {
            usage.pricing_snapshot = Some(self.clone());
            return Ok(usage);
        };
        if cache_usage.write.is_some() && self.cache_write_per_unit.is_none() {
            usage.pricing_snapshot = Some(self.clone());
            return Ok(usage);
        }

        let unit = self.unit_tokens as f64;
        let write_cost = match (&cache_usage.write, self.cache_write_per_unit) {
            (Some(write), Some(price)) => write.tokens as f64 * price / unit,
            (None, _) => 0.0,
            (Some(_), None) => unreachable!("missing write price returned above"),
        };
        usage.input_cost = round_usage_cost(
            read.tokens as f64 * self.cache_read_per_unit / unit
                + uncached.tokens as f64 * self.uncached_input_per_unit / unit
                + write_cost,
        );
        usage.output_cost =
            round_usage_cost(usage.completion_tokens as f64 * self.output_per_unit / unit);
        usage.cache_savings = round_usage_cost(
            read.tokens as f64 * (self.uncached_input_per_unit - self.cache_read_per_unit) / unit,
        );
        usage.pricing_snapshot = Some(self.clone());
        Ok(usage)
    }
}

fn round_usage_cost(value: f64) -> f64 {
    const COST_DECIMAL_SCALE: f64 = 1_000_000_000.0;
    (value * COST_DECIMAL_SCALE).round() / COST_DECIMAL_SCALE
}

pub const PREFIX_SNAPSHOT_MATERIALIZATION_SCHEMA_VERSION: u16 = 2;
pub const PREFIX_SNAPSHOT_CONTEXT_ITEM_LIMIT: usize = 8;

/// Bounded metadata for one deterministic prefix materialization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PrefixSnapshotMaterialization {
    pub schema_version: u16,
    pub byte_len: usize,
    pub message_count: usize,
    pub tool_schema_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_context: Option<PrefixRuntimeContextSummary>,
}

impl PrefixSnapshotMaterialization {
    #[must_use]
    pub fn new(
        byte_len: usize,
        message_count: usize,
        tool_schema_count: usize,
        runtime_context: Option<PrefixRuntimeContextSummary>,
    ) -> Self {
        Self {
            schema_version: PREFIX_SNAPSHOT_MATERIALIZATION_SCHEMA_VERSION,
            byte_len,
            message_count,
            tool_schema_count,
            runtime_context,
        }
    }
}

/// Bounded, content-free provenance for the runtime context included in one prefix.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PrefixRuntimeContextSummary {
    pub schema: String,
    pub max_tokens: usize,
    pub used_tokens: usize,
    pub included_count: usize,
    pub excluded_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_included: Vec<PrefixRuntimeContextItemSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_by_reason: Vec<PrefixRuntimeContextExclusionSummary>,
}

/// One content-free runtime-context row retained for the TUI provenance surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PrefixRuntimeContextItemSummary {
    pub source: crate::ContextSource,
    pub inclusion_reason: crate::ContextInclusionReason,
    pub token_cost: usize,
}

/// Count of runtime-context candidates excluded for one normalized reason.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PrefixRuntimeContextExclusionSummary {
    pub reason: crate::ContextInclusionReason,
    pub count: usize,
}

/// Stable snapshot of deterministic prefix identity for auditing and resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PrefixSnapshot {
    pub materialization: PrefixSnapshotMaterialization,
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
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub cache_write_observed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cache_layout_mutation: Option<crate::CacheLayoutMutationKind>,
    #[serde(default)]
    pub last_provider_miss_without_local_mutation: bool,
}

impl SessionStats {
    /// Merges one request's usage counters into the running session totals.
    pub fn apply_usage(&mut self, usage: &UsageStats) {
        self.apply_usage_totals(usage);
        self.last_prompt_tokens = usage.prompt_tokens;
        if let Some(cache_usage) = &usage.cache_usage {
            if let Some(mutation) = cache_usage.local_layout_mutation {
                self.last_cache_layout_mutation = Some(mutation);
            }
            self.last_provider_miss_without_local_mutation =
                cache_usage.provider_miss_without_local_mutation;
        }
    }

    /// Merges an internal compactor request into session billing totals without replacing the
    /// latest ordinary conversation-turn pressure/cache observation.
    pub fn apply_semantic_compaction_usage(&mut self, usage: &UsageStats) {
        self.apply_usage_totals(usage);
    }

    fn apply_usage_totals(&mut self, usage: &UsageStats) {
        self.prompt_tokens += usage.prompt_tokens;
        self.completion_tokens += usage.completion_tokens;
        self.cache_hit_tokens += usage.cache_hit_tokens;
        self.cache_miss_tokens += usage.cache_miss_tokens;
        self.input_cost += usage.input_cost;
        self.output_cost += usage.output_cost;
        self.cache_savings += usage.cache_savings;
        if let Some(cache_usage) = &usage.cache_usage
            && let Some(write) = &cache_usage.write
        {
            self.cache_write_tokens = self.cache_write_tokens.saturating_add(write.tokens);
            self.cache_write_observed = true;
        }
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// Returns the stable provider registry name.
    fn name(&self) -> &str;

    /// Returns the provider's declared runtime capabilities.
    fn capabilities(&self) -> ProviderCapabilities;

    /// Returns the context/cache contract for one model on this exact configured route.
    ///
    /// The default fails closed. Compatible adapters may expose observed counters, but must not
    /// infer vendor-only cache or native-continuation behavior from a model identifier.
    fn context_capabilities(&self, _model_name: &str) -> ProviderContextCapabilities {
        ProviderContextCapabilities::unknown()
    }

    /// Returns trusted, exact-model pricing evidence for usage accounting.
    ///
    /// Unknown or compatible routes return `None`; callers must never treat absence as zero cost.
    fn usage_pricing_snapshot(&self, _model_name: &str) -> Option<ModelPricingSnapshotV1> {
        None
    }

    /// Optionally dual-writes a provider-native acceleration carrier after the portable
    /// checkpoint is already durable.
    ///
    /// The default fails closed. Implementations must use the kernel native-compaction physical
    /// attempt and encrypted payload lifecycle; returning a carrier must never activate it or
    /// replace portable continuity truth.
    async fn materialize_native_compaction_carrier(
        &self,
        _session: &crate::Session,
        _logical_run_id: String,
        _frozen_request: crate::FrozenProviderRequestMaterial,
        _covers_through: crate::CompactionCursor,
        _portable_compaction_id: crate::CompactionId,
        _carrier_policy: crate::NativeCarrierPolicyV1,
    ) -> Result<Option<crate::NativeProviderCompactionMaterialization>> {
        anyhow::bail!("provider-native compaction carrier is unavailable on this route")
    }

    /// Returns native hosted web-search support for one exact model id.
    fn hosted_web_search_capability(&self, _model_name: &str) -> crate::HostedWebSearchCapability {
        crate::HostedWebSearchCapability::default()
    }

    /// Returns exact-model image input support. Compatible or unknown providers fail closed by
    /// default instead of inferring multimodal support from a text protocol shape.
    fn image_input_capability(&self, _model_name: &str) -> crate::ImageInputCapability {
        crate::ImageInputCapability::Unsupported
    }

    /// Classifies an adapter-proven request rejection or transport setup failure that happened
    /// before any model generation or side effect.
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

    fn context_capabilities(&self, model_name: &str) -> ProviderContextCapabilities {
        (**self).context_capabilities(model_name)
    }

    fn usage_pricing_snapshot(&self, model_name: &str) -> Option<ModelPricingSnapshotV1> {
        (**self).usage_pricing_snapshot(model_name)
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

/// A provider-adapter rejection fact expressed without leaking transport or provider error types
/// into the kernel. Every variant proves that model generation could not have started.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRequestRejection {
    ContextWindowExceeded,
    /// The provider adapter proved that transport setup failed before any application request
    /// bytes could be dispatched.
    ConnectFailedBeforeDispatch,
    /// Provider-owned HTTP 429 or an already-active route cooldown rejected the request before
    /// model generation.
    RateLimited,
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

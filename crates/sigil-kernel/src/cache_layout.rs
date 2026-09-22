use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{CompletionRequest, ImageAttachment, MessageRole, ModelMessage, ToolCall};

/// Schema version for the legacy provider-neutral, hash-only request cache layout proof.
pub const CACHE_LAYOUT_PROOF_SCHEMA_VERSION: u16 = 1;

/// Schema version for semantic provider-wire cache layout proofs.
pub const CACHE_LAYOUT_PROOF_V2_SCHEMA_VERSION: u16 = 2;

/// Maximum number of per-message fingerprints retained for bounded rewrite diagnostics.
const CACHE_LAYOUT_MESSAGE_FINGERPRINT_LIMIT: usize = 128;

/// Returns a recursively key-sorted JSON value suitable for cache-stable provider wire material.
///
/// Arrays retain their semantic order. Numbers use the same normalization as durable event
/// hashing, so equivalent tool schemas do not churn a provider prefix because their object keys
/// were inserted in a different order.
///
/// # Errors
///
/// Returns an error when a number cannot be represented by the canonical JSON profile.
pub fn canonicalize_cache_stable_json(value: &serde_json::Value) -> Result<serde_json::Value> {
    let bytes = crate::event::canonical_json_bytes(value)?;
    serde_json::from_slice(&bytes).context("failed to decode canonical cache-stable JSON")
}

/// Why the current logical provider request differs from the previous physical attempt.
///
/// The reason proves only local request material changes. A provider can still miss its cache
/// when this value is [`Self::Identical`] or [`Self::ConversationTailAppended`]; TTL expiry,
/// eviction and provider-side routing remain unproven in that case.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheLayoutMutationKind {
    FirstObservation,
    Identical,
    RouteChanged,
    SystemChanged,
    ToolSchemaChanged,
    ConversationHistoryRewritten,
    ConversationTailAppended,
    DynamicStateOnly,
}

impl CacheLayoutMutationKind {
    /// Stable user-facing diagnostic label that does not expose provider-private terminology.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FirstObservation => "first_observation",
            Self::Identical => "identical",
            Self::RouteChanged => "route_changed",
            Self::SystemChanged => "system_changed",
            Self::ToolSchemaChanged => "tool_schema_changed",
            Self::ConversationHistoryRewritten => "conversation_history_rewritten",
            Self::ConversationTailAppended => "conversation_tail_appended",
            Self::DynamicStateOnly => "dynamic_state_only",
        }
    }
}

/// Comparison evidence between two consecutive request cache layouts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CacheLayoutMutationProofV1 {
    pub kind: CacheLayoutMutationKind,
    /// Number of earlier conversation messages proven byte-identical at the logical layer.
    pub reusable_conversation_message_count: u64,
    /// Whether the local stable prefix bytes are unchanged. This does not claim a provider hit.
    pub local_stable_prefix_preserved: bool,
    /// Bounded provider-visible fingerprints for the current conversation. These contain only
    /// hashes, roles and sizes, so diagnostics can identify a changed region without persisting
    /// message content.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_fingerprints: Vec<CacheLayoutMessageFingerprintV1>,
    /// Provider-visible byte total for the current conversation material.
    #[serde(default)]
    pub conversation_material_bytes: u64,
    /// Bounded explanation of the mutation, when a previous layout exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<CacheLayoutMutationDiagnosticV1>,
}

/// Hash-only fingerprint for one provider-visible conversation message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CacheLayoutMessageFingerprintV1 {
    pub role: MessageRole,
    pub content_hash: String,
    pub tool_calls_hash: String,
    pub tool_call_id_hash: String,
    pub image_attachments_hash: String,
    pub tool_result_payload_hash: String,
    pub provider_visible_bytes: u64,
}

/// Bounded, content-free explanation of a cache layout mutation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CacheLayoutMutationDiagnosticV1 {
    pub previous_message_count: u64,
    pub current_message_count: u64,
    pub first_changed_message_index: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_fields: Vec<CacheLayoutMessageFieldV1>,
    pub previous_conversation_material_bytes: u64,
    pub current_conversation_material_bytes: u64,
    pub compared_message_count: u64,
    pub comparison_truncated: bool,
}

/// A field category used by cache rewrite diagnostics. It never contains message content.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheLayoutMessageFieldV1 {
    Role,
    Content,
    ToolCalls,
    ToolCallId,
    ImageAttachments,
    ToolResultPayload,
    MessageInserted,
    MessageRemoved,
    Size,
}

/// Hash-only proof of the provider-neutral request shape frozen before provider I/O.
///
/// Hashes use deterministic, domain-separated SHA-256 so they remain comparable after restart.
/// Raw messages, tool schemas, paths, continuation payloads and partition keys are never stored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CacheLayoutProofV1 {
    pub schema_version: u16,
    pub layout_hash: String,
    pub route_hash: String,
    pub system_hash: String,
    pub tool_schema_hash: String,
    pub conversation_hash: String,
    pub dynamic_state_hash: String,
    pub system_message_count: u64,
    pub tool_count: u64,
    pub conversation_message_count: u64,
    pub mutation_from_previous: CacheLayoutMutationProofV1,
}

impl CacheLayoutProofV1 {
    /// Materializes one deterministic proof and compares it with the prior durable attempt.
    ///
    /// # Errors
    ///
    /// Returns an error when request subsets cannot be represented as canonical JSON or counts
    /// exceed the durable `u64` representation.
    pub fn from_request(request: &CompletionRequest, previous: Option<&Self>) -> Result<Self> {
        let leading_system_count = request
            .messages
            .iter()
            .take_while(|message| message.role == MessageRole::System)
            .count();
        let (system_messages, conversation_messages) =
            request.messages.split_at(leading_system_count);
        let route_hash = hash_canonical(
            "sigil-cache-layout-route-v1",
            &(request.provider_name.as_str(), request.model_name.as_str()),
        )?;
        let system_hash = hash_canonical("sigil-cache-layout-system-v1", &system_messages)?;
        let tool_schema_hash = hash_canonical(
            "sigil-cache-layout-tools-v1",
            &ToolSchemaMaterialV1 {
                local_tools: &request.tools,
                hosted_tools: &request.hosted_tools,
            },
        )?;
        let conversation_hash =
            hash_canonical("sigil-cache-layout-conversation-v1", &conversation_messages)?;
        let conversation_fingerprints = conversation_messages
            .iter()
            .map(message_fingerprint)
            .collect::<Result<Vec<_>>>()?;
        let conversation_material_bytes = canonical_material_bytes(conversation_messages)?;
        let dynamic_state_hash = hash_canonical(
            "sigil-cache-layout-dynamic-v1",
            &DynamicRequestMaterialV1::from(request),
        )?;

        let system_message_count = durable_count(system_messages.len(), "system messages")?;
        let tool_count = durable_count(
            request
                .tools
                .len()
                .saturating_add(request.hosted_tools.len()),
            "tools",
        )?;
        let conversation_message_count =
            durable_count(conversation_messages.len(), "conversation messages")?;
        let mutation_from_previous = mutation_from_previous(
            previous,
            &route_hash,
            &system_hash,
            &tool_schema_hash,
            &conversation_hash,
            &dynamic_state_hash,
            conversation_messages,
            &conversation_fingerprints,
            conversation_material_bytes,
        )?;
        let layout_hash = hash_canonical(
            "sigil-cache-layout-proof-v1",
            &LayoutIdentityV1 {
                route_hash: &route_hash,
                system_hash: &system_hash,
                tool_schema_hash: &tool_schema_hash,
                conversation_hash: &conversation_hash,
                dynamic_state_hash: &dynamic_state_hash,
                system_message_count,
                tool_count,
                conversation_message_count,
            },
        )?;

        Ok(Self {
            schema_version: CACHE_LAYOUT_PROOF_SCHEMA_VERSION,
            layout_hash,
            route_hash,
            system_hash,
            tool_schema_hash,
            conversation_hash,
            dynamic_state_hash,
            system_message_count,
            tool_count,
            conversation_message_count,
            mutation_from_previous,
        })
    }

    /// Validates the bounded, hash-only durable representation.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported schema versions, malformed hashes or inconsistent first
    /// observation evidence.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CACHE_LAYOUT_PROOF_SCHEMA_VERSION {
            bail!(
                "unsupported cache layout proof schema version {}",
                self.schema_version
            );
        }
        for (label, hash) in [
            ("layout", &self.layout_hash),
            ("route", &self.route_hash),
            ("system", &self.system_hash),
            ("tool schema", &self.tool_schema_hash),
            ("conversation", &self.conversation_hash),
            ("dynamic state", &self.dynamic_state_hash),
        ] {
            if !is_sha256(hash) {
                bail!("cache layout {label} hash is malformed");
            }
        }
        if self.mutation_from_previous.kind == CacheLayoutMutationKind::FirstObservation
            && (self
                .mutation_from_previous
                .reusable_conversation_message_count
                != 0
                || self.mutation_from_previous.local_stable_prefix_preserved)
        {
            bail!("cache layout first observation cannot claim reusable prefix evidence");
        }
        if self
            .mutation_from_previous
            .reusable_conversation_message_count
            > self.conversation_message_count
        {
            bail!("cache layout reusable conversation count exceeds the current request");
        }
        if self.mutation_from_previous.message_fingerprints.len()
            > CACHE_LAYOUT_MESSAGE_FINGERPRINT_LIMIT
        {
            bail!("cache layout message fingerprint list exceeds bounded limit");
        }
        if self.mutation_from_previous.message_fingerprints.len() as u64
            > self.conversation_message_count
        {
            bail!("cache layout message fingerprints exceed the current conversation count");
        }
        Ok(())
    }
}

/// Hash-only proof of provider-visible request semantics frozen before provider I/O.
///
/// Unlike [`CacheLayoutProofV1`], hosted-tool authorization IDs and request fingerprints do not
/// participate in the tool-schema identity because providers never receive them as declaration
/// fields. The legacy proof remains readable so existing sessions can replay without migration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CacheLayoutProofV2 {
    pub schema_version: u16,
    pub layout_hash: String,
    pub route_hash: String,
    pub system_hash: String,
    pub tool_schema_hash: String,
    pub conversation_hash: String,
    pub dynamic_state_hash: String,
    pub system_message_count: u64,
    pub tool_count: u64,
    pub conversation_message_count: u64,
    pub mutation_from_previous: CacheLayoutMutationProofV1,
}

impl CacheLayoutProofV2 {
    /// Materializes one deterministic semantic proof and compares it with a prior V2 proof.
    ///
    /// # Errors
    ///
    /// Returns an error when request subsets cannot be represented as canonical JSON, hosted
    /// declarations are invalid or counts exceed the durable `u64` representation.
    pub fn from_request(request: &CompletionRequest, previous: Option<&Self>) -> Result<Self> {
        let leading_system_count = request
            .messages
            .iter()
            .take_while(|message| message.role == MessageRole::System)
            .count();
        let (system_messages, conversation_messages) =
            request.messages.split_at(leading_system_count);
        let conversation_material = conversation_messages
            .iter()
            .map(ProviderVisibleMessageMaterial::from)
            .collect::<Vec<_>>();
        let conversation_fingerprints = conversation_material
            .iter()
            .map(provider_visible_message_fingerprint)
            .collect::<Result<Vec<_>>>()?;
        let hosted_tools = request
            .hosted_tools
            .iter()
            .map(crate::hosted::HostedToolRequest::semantic_declaration)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let local_tools = request
            .tools
            .iter()
            .map(LocalToolDeclarationV2::from)
            .collect::<Vec<_>>();

        let route_hash = hash_canonical(
            "sigil-cache-layout-route-v2",
            &(request.provider_name.as_str(), request.model_name.as_str()),
        )?;
        let system_hash = hash_canonical("sigil-cache-layout-system-v2", &system_messages)?;
        let tool_schema_hash = hash_canonical(
            "sigil-cache-layout-tools-v2",
            &ToolSchemaMaterialV2 {
                local_tools: &local_tools,
                hosted_tools: &hosted_tools,
            },
        )?;
        let conversation_hash =
            hash_canonical("sigil-cache-layout-conversation-v2", &conversation_material)?;
        let conversation_material_bytes = canonical_material_bytes(&conversation_material)?;
        let dynamic_state_hash = hash_canonical(
            "sigil-cache-layout-dynamic-v2",
            &DynamicRequestMaterialV1::from(request),
        )?;

        let system_message_count = durable_count(system_messages.len(), "system messages")?;
        let tool_count = durable_count(
            request
                .tools
                .len()
                .saturating_add(request.hosted_tools.len()),
            "tools",
        )?;
        let conversation_message_count =
            durable_count(conversation_messages.len(), "conversation messages")?;
        let mutation_from_previous = mutation_from_previous_v2(
            previous,
            &route_hash,
            &system_hash,
            &tool_schema_hash,
            &conversation_hash,
            &dynamic_state_hash,
            &conversation_material,
            &conversation_fingerprints,
            conversation_material_bytes,
        )?;
        let layout_hash = hash_canonical(
            "sigil-cache-layout-proof-v2",
            &LayoutIdentityV1 {
                route_hash: &route_hash,
                system_hash: &system_hash,
                tool_schema_hash: &tool_schema_hash,
                conversation_hash: &conversation_hash,
                dynamic_state_hash: &dynamic_state_hash,
                system_message_count,
                tool_count,
                conversation_message_count,
            },
        )?;

        Ok(Self {
            schema_version: CACHE_LAYOUT_PROOF_V2_SCHEMA_VERSION,
            layout_hash,
            route_hash,
            system_hash,
            tool_schema_hash,
            conversation_hash,
            dynamic_state_hash,
            system_message_count,
            tool_count,
            conversation_message_count,
            mutation_from_previous,
        })
    }

    /// Validates the bounded V2 hash-only durable representation.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported schema versions, malformed hashes or inconsistent
    /// mutation evidence.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CACHE_LAYOUT_PROOF_V2_SCHEMA_VERSION {
            bail!(
                "unsupported cache layout proof V2 schema version {}",
                self.schema_version
            );
        }
        for (label, hash) in [
            ("layout", &self.layout_hash),
            ("route", &self.route_hash),
            ("system", &self.system_hash),
            ("tool schema", &self.tool_schema_hash),
            ("conversation", &self.conversation_hash),
            ("dynamic state", &self.dynamic_state_hash),
        ] {
            if !is_sha256(hash) {
                bail!("cache layout {label} hash is malformed");
            }
        }
        if self.mutation_from_previous.kind == CacheLayoutMutationKind::FirstObservation
            && (self
                .mutation_from_previous
                .reusable_conversation_message_count
                != 0
                || self.mutation_from_previous.local_stable_prefix_preserved)
        {
            bail!("cache layout first observation cannot claim reusable prefix evidence");
        }
        if self
            .mutation_from_previous
            .reusable_conversation_message_count
            > self.conversation_message_count
        {
            bail!("cache layout reusable conversation count exceeds the current request");
        }
        if self.mutation_from_previous.message_fingerprints.len()
            > CACHE_LAYOUT_MESSAGE_FINGERPRINT_LIMIT
        {
            bail!("cache layout message fingerprint list exceeds bounded limit");
        }
        if self.mutation_from_previous.message_fingerprints.len() as u64
            > self.conversation_message_count
        {
            bail!("cache layout message fingerprints exceed the current conversation count");
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct ToolSchemaMaterialV1<'a> {
    local_tools: &'a [crate::ToolSpec],
    hosted_tools: &'a [crate::HostedToolRequest],
}

#[derive(Serialize)]
struct LocalToolDeclarationV2<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: &'a serde_json::Value,
}

impl<'a> From<&'a crate::ToolSpec> for LocalToolDeclarationV2<'a> {
    fn from(tool: &'a crate::ToolSpec) -> Self {
        Self {
            name: &tool.name,
            description: &tool.description,
            input_schema: &tool.input_schema,
        }
    }
}

#[derive(Serialize)]
struct ToolSchemaMaterialV2<'a> {
    local_tools: &'a [LocalToolDeclarationV2<'a>],
    hosted_tools: &'a [crate::hosted::HostedToolDeclarationV1],
}

#[derive(Serialize)]
struct DynamicRequestMaterialV1<'a> {
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    reasoning_effort: Option<&'a crate::ReasoningEffort>,
    previous_response_handle: Option<&'a crate::ResponseHandle>,
    continuation_states: &'a [crate::ProviderContinuationState],
    traffic_partition_key: Option<&'a str>,
    background: bool,
    store: bool,
    deterministic_materialization: bool,
}

impl<'a> From<&'a CompletionRequest> for DynamicRequestMaterialV1<'a> {
    fn from(request: &'a CompletionRequest) -> Self {
        Self {
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            reasoning_effort: request.reasoning_effort.as_ref(),
            previous_response_handle: request.previous_response_handle.as_ref(),
            continuation_states: &request.continuation_states,
            traffic_partition_key: request.traffic_partition_key.as_deref(),
            background: request.background,
            store: request.store,
            deterministic_materialization: request.deterministic_materialization,
        }
    }
}

#[derive(Serialize)]
struct LayoutIdentityV1<'a> {
    route_hash: &'a str,
    system_hash: &'a str,
    tool_schema_hash: &'a str,
    conversation_hash: &'a str,
    dynamic_state_hash: &'a str,
    system_message_count: u64,
    tool_count: u64,
    conversation_message_count: u64,
}

/// Message material that can affect a provider request. Durable host bookkeeping such as the
/// local message id, logical run id and assistant presentation kind is deliberately excluded.
#[derive(Serialize)]
struct ProviderVisibleMessageMaterial {
    role: MessageRole,
    content: Option<String>,
    tool_calls: Vec<ToolCall>,
    tool_call_id: Option<String>,
    image_attachments: Vec<ImageAttachment>,
    tool_result_payload: Option<crate::session::ProviderToolResultMessageV1>,
}

impl From<&ModelMessage> for ProviderVisibleMessageMaterial {
    fn from(message: &ModelMessage) -> Self {
        Self {
            role: message.role.clone(),
            content: message.content.clone(),
            tool_calls: message.tool_calls.clone(),
            tool_call_id: message.tool_call_id.clone(),
            image_attachments: message.image_attachments.clone(),
            tool_result_payload: message.tool_result_payload.clone(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn mutation_from_previous(
    previous: Option<&CacheLayoutProofV1>,
    route_hash: &str,
    system_hash: &str,
    tool_schema_hash: &str,
    conversation_hash: &str,
    dynamic_state_hash: &str,
    conversation_messages: &[crate::ModelMessage],
    conversation_fingerprints: &[CacheLayoutMessageFingerprintV1],
    conversation_material_bytes: u64,
) -> Result<CacheLayoutMutationProofV1> {
    let Some(previous) = previous else {
        return Ok(CacheLayoutMutationProofV1 {
            kind: CacheLayoutMutationKind::FirstObservation,
            reusable_conversation_message_count: 0,
            local_stable_prefix_preserved: false,
            message_fingerprints: bounded_fingerprints(conversation_fingerprints),
            conversation_material_bytes,
            diagnostic: Some(cache_layout_mutation_diagnostic(
                None,
                0,
                conversation_fingerprints,
                conversation_messages.len() as u64,
                0,
                conversation_material_bytes,
            )),
        });
    };
    previous.validate()?;
    let previous_conversation_count = usize::try_from(previous.conversation_message_count)
        .context("prior cache layout conversation count exceeds usize")?;
    let previous_fingerprints = &previous.mutation_from_previous.message_fingerprints;
    let full_prefix_preserved = previous_conversation_count <= conversation_messages.len()
        && hash_canonical(
            "sigil-cache-layout-conversation-v1",
            &conversation_messages[..previous_conversation_count],
        )? == previous.conversation_hash;
    let reusable_conversation_message_count = if full_prefix_preserved {
        previous.conversation_message_count
    } else if !previous_fingerprints.is_empty() {
        common_fingerprint_prefix_count(previous_fingerprints, conversation_fingerprints)
            .min(previous.conversation_message_count)
    } else {
        0
    };

    let kind = if route_hash != previous.route_hash {
        CacheLayoutMutationKind::RouteChanged
    } else if system_hash != previous.system_hash {
        CacheLayoutMutationKind::SystemChanged
    } else if tool_schema_hash != previous.tool_schema_hash {
        CacheLayoutMutationKind::ToolSchemaChanged
    } else if conversation_hash != previous.conversation_hash {
        if reusable_conversation_message_count == previous.conversation_message_count
            && conversation_messages.len() > previous_conversation_count
        {
            CacheLayoutMutationKind::ConversationTailAppended
        } else {
            CacheLayoutMutationKind::ConversationHistoryRewritten
        }
    } else if dynamic_state_hash != previous.dynamic_state_hash {
        CacheLayoutMutationKind::DynamicStateOnly
    } else {
        CacheLayoutMutationKind::Identical
    };
    let local_stable_prefix_preserved = matches!(
        kind,
        CacheLayoutMutationKind::Identical
            | CacheLayoutMutationKind::ConversationTailAppended
            | CacheLayoutMutationKind::DynamicStateOnly
    );
    Ok(CacheLayoutMutationProofV1 {
        kind,
        reusable_conversation_message_count,
        local_stable_prefix_preserved,
        message_fingerprints: bounded_fingerprints(conversation_fingerprints),
        conversation_material_bytes,
        diagnostic: Some(cache_layout_mutation_diagnostic(
            Some(previous_fingerprints),
            previous.conversation_message_count,
            conversation_fingerprints,
            conversation_messages.len() as u64,
            previous.mutation_from_previous.conversation_material_bytes,
            conversation_material_bytes,
        )),
    })
}

#[allow(clippy::too_many_arguments)]
fn mutation_from_previous_v2(
    previous: Option<&CacheLayoutProofV2>,
    route_hash: &str,
    system_hash: &str,
    tool_schema_hash: &str,
    conversation_hash: &str,
    dynamic_state_hash: &str,
    conversation_messages: &[ProviderVisibleMessageMaterial],
    conversation_fingerprints: &[CacheLayoutMessageFingerprintV1],
    conversation_material_bytes: u64,
) -> Result<CacheLayoutMutationProofV1> {
    let Some(previous) = previous else {
        return Ok(CacheLayoutMutationProofV1 {
            kind: CacheLayoutMutationKind::FirstObservation,
            reusable_conversation_message_count: 0,
            local_stable_prefix_preserved: false,
            message_fingerprints: bounded_fingerprints(conversation_fingerprints),
            conversation_material_bytes,
            diagnostic: Some(cache_layout_mutation_diagnostic(
                None,
                0,
                conversation_fingerprints,
                conversation_messages.len() as u64,
                0,
                conversation_material_bytes,
            )),
        });
    };
    previous.validate()?;
    let previous_conversation_count = usize::try_from(previous.conversation_message_count)
        .context("prior cache layout conversation count exceeds usize")?;
    let previous_fingerprints = &previous.mutation_from_previous.message_fingerprints;
    let full_prefix_preserved = previous_conversation_count <= conversation_messages.len()
        && hash_canonical(
            "sigil-cache-layout-conversation-v2",
            &conversation_messages[..previous_conversation_count],
        )? == previous.conversation_hash;
    let reusable_conversation_message_count = if full_prefix_preserved {
        previous.conversation_message_count
    } else if !previous_fingerprints.is_empty() {
        common_fingerprint_prefix_count(previous_fingerprints, conversation_fingerprints)
            .min(previous.conversation_message_count)
    } else {
        0
    };

    let kind = if route_hash != previous.route_hash {
        CacheLayoutMutationKind::RouteChanged
    } else if system_hash != previous.system_hash {
        CacheLayoutMutationKind::SystemChanged
    } else if tool_schema_hash != previous.tool_schema_hash {
        CacheLayoutMutationKind::ToolSchemaChanged
    } else if conversation_hash != previous.conversation_hash {
        if reusable_conversation_message_count == previous.conversation_message_count
            && conversation_messages.len() > previous_conversation_count
        {
            CacheLayoutMutationKind::ConversationTailAppended
        } else {
            CacheLayoutMutationKind::ConversationHistoryRewritten
        }
    } else if dynamic_state_hash != previous.dynamic_state_hash {
        CacheLayoutMutationKind::DynamicStateOnly
    } else {
        CacheLayoutMutationKind::Identical
    };
    let local_stable_prefix_preserved = matches!(
        kind,
        CacheLayoutMutationKind::Identical
            | CacheLayoutMutationKind::ConversationTailAppended
            | CacheLayoutMutationKind::DynamicStateOnly
    );
    Ok(CacheLayoutMutationProofV1 {
        kind,
        reusable_conversation_message_count,
        local_stable_prefix_preserved,
        message_fingerprints: bounded_fingerprints(conversation_fingerprints),
        conversation_material_bytes,
        diagnostic: Some(cache_layout_mutation_diagnostic(
            Some(previous_fingerprints),
            previous.conversation_message_count,
            conversation_fingerprints,
            conversation_messages.len() as u64,
            previous.mutation_from_previous.conversation_material_bytes,
            conversation_material_bytes,
        )),
    })
}

fn bounded_fingerprints(
    fingerprints: &[CacheLayoutMessageFingerprintV1],
) -> Vec<CacheLayoutMessageFingerprintV1> {
    fingerprints
        .iter()
        .take(CACHE_LAYOUT_MESSAGE_FINGERPRINT_LIMIT)
        .cloned()
        .collect()
}

fn common_fingerprint_prefix_count(
    previous: &[CacheLayoutMessageFingerprintV1],
    current: &[CacheLayoutMessageFingerprintV1],
) -> u64 {
    previous
        .iter()
        .zip(current)
        .take_while(|(previous, current)| *previous == *current)
        .count() as u64
}

fn cache_layout_mutation_diagnostic(
    previous_fingerprints: Option<&[CacheLayoutMessageFingerprintV1]>,
    previous_message_count: u64,
    current_fingerprints: &[CacheLayoutMessageFingerprintV1],
    current_message_count: u64,
    previous_conversation_material_bytes: u64,
    current_conversation_material_bytes: u64,
) -> CacheLayoutMutationDiagnosticV1 {
    let previous_fingerprints = previous_fingerprints.unwrap_or_default();
    let compared_message_count = previous_fingerprints.len().min(current_fingerprints.len()) as u64;
    let first_changed_message_index = (0..compared_message_count as usize)
        .find(|index| previous_fingerprints[*index] != current_fingerprints[*index])
        .map(|index| index as u64)
        .or_else(|| {
            (previous_fingerprints.len() != current_fingerprints.len())
                .then_some(compared_message_count)
        });
    let mut changed_fields = first_changed_message_index
        .and_then(|index| {
            let index = usize::try_from(index).ok()?;
            match (
                previous_fingerprints.get(index),
                current_fingerprints.get(index),
            ) {
                (Some(previous), Some(current)) => Some(message_changed_fields(previous, current)),
                (None, Some(_)) => Some(vec![CacheLayoutMessageFieldV1::MessageInserted]),
                (Some(_), None) => Some(vec![CacheLayoutMessageFieldV1::MessageRemoved]),
                (None, None) => None,
            }
        })
        .unwrap_or_default();
    if previous_message_count != current_message_count
        && !changed_fields.iter().any(|field| {
            matches!(
                field,
                CacheLayoutMessageFieldV1::MessageInserted
                    | CacheLayoutMessageFieldV1::MessageRemoved
            )
        })
    {
        changed_fields.push(if current_message_count > previous_message_count {
            CacheLayoutMessageFieldV1::MessageInserted
        } else {
            CacheLayoutMessageFieldV1::MessageRemoved
        });
    }
    if previous_conversation_material_bytes != current_conversation_material_bytes
        && !changed_fields.contains(&CacheLayoutMessageFieldV1::Size)
    {
        changed_fields.push(CacheLayoutMessageFieldV1::Size);
    }
    CacheLayoutMutationDiagnosticV1 {
        previous_message_count,
        current_message_count,
        first_changed_message_index,
        changed_fields,
        previous_conversation_material_bytes,
        current_conversation_material_bytes,
        compared_message_count,
        comparison_truncated: previous_message_count as usize > previous_fingerprints.len()
            || current_message_count as usize > CACHE_LAYOUT_MESSAGE_FINGERPRINT_LIMIT,
    }
}

fn message_changed_fields(
    previous: &CacheLayoutMessageFingerprintV1,
    current: &CacheLayoutMessageFingerprintV1,
) -> Vec<CacheLayoutMessageFieldV1> {
    let mut fields = Vec::new();
    if previous.role != current.role {
        fields.push(CacheLayoutMessageFieldV1::Role);
    }
    if previous.content_hash != current.content_hash {
        fields.push(CacheLayoutMessageFieldV1::Content);
    }
    if previous.tool_calls_hash != current.tool_calls_hash {
        fields.push(CacheLayoutMessageFieldV1::ToolCalls);
    }
    if previous.tool_call_id_hash != current.tool_call_id_hash {
        fields.push(CacheLayoutMessageFieldV1::ToolCallId);
    }
    if previous.image_attachments_hash != current.image_attachments_hash {
        fields.push(CacheLayoutMessageFieldV1::ImageAttachments);
    }
    if previous.tool_result_payload_hash != current.tool_result_payload_hash {
        fields.push(CacheLayoutMessageFieldV1::ToolResultPayload);
    }
    if previous.provider_visible_bytes != current.provider_visible_bytes {
        fields.push(CacheLayoutMessageFieldV1::Size);
    }
    fields
}

fn message_fingerprint(message: &ModelMessage) -> Result<CacheLayoutMessageFingerprintV1> {
    Ok(CacheLayoutMessageFingerprintV1 {
        role: message.role.clone(),
        content_hash: hash_canonical("sigil-cache-layout-message-content-v1", &message.content)?,
        tool_calls_hash: hash_canonical(
            "sigil-cache-layout-message-tool-calls-v1",
            &message.tool_calls,
        )?,
        tool_call_id_hash: hash_canonical(
            "sigil-cache-layout-message-tool-call-id-v1",
            &message.tool_call_id,
        )?,
        image_attachments_hash: hash_canonical(
            "sigil-cache-layout-message-images-v1",
            &message.image_attachments,
        )?,
        tool_result_payload_hash: hash_canonical(
            "sigil-cache-layout-message-tool-result-v1",
            &message.tool_result_payload,
        )?,
        provider_visible_bytes: canonical_material_bytes(message)?,
    })
}

fn provider_visible_message_fingerprint(
    message: &ProviderVisibleMessageMaterial,
) -> Result<CacheLayoutMessageFingerprintV1> {
    Ok(CacheLayoutMessageFingerprintV1 {
        role: message.role.clone(),
        content_hash: hash_canonical("sigil-cache-layout-message-content-v2", &message.content)?,
        tool_calls_hash: hash_canonical(
            "sigil-cache-layout-message-tool-calls-v2",
            &message.tool_calls,
        )?,
        tool_call_id_hash: hash_canonical(
            "sigil-cache-layout-message-tool-call-id-v2",
            &message.tool_call_id,
        )?,
        image_attachments_hash: hash_canonical(
            "sigil-cache-layout-message-images-v2",
            &message.image_attachments,
        )?,
        tool_result_payload_hash: hash_canonical(
            "sigil-cache-layout-message-tool-result-v2",
            &message.tool_result_payload,
        )?,
        provider_visible_bytes: canonical_material_bytes(message)?,
    })
}

fn canonical_material_bytes<T: Serialize + ?Sized>(value: &T) -> Result<u64> {
    let value = serde_json::to_value(value).context("failed to serialize cache material")?;
    let bytes = crate::event::canonical_json_bytes(&value)
        .context("failed to canonicalize cache material")?;
    u64::try_from(bytes.len()).context("cache material size exceeds u64")
}

fn hash_canonical<T>(domain: &str, value: &T) -> Result<String>
where
    T: Serialize + ?Sized,
{
    let value = serde_json::to_value(value)
        .with_context(|| format!("failed to serialize {domain} material"))?;
    let bytes = crate::event::canonical_json_bytes(&value)
        .with_context(|| format!("failed to canonicalize {domain} material"))?;
    let mut digest = Sha256::new();
    digest.update((domain.len() as u64).to_be_bytes());
    digest.update(domain.as_bytes());
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn durable_count(count: usize, label: &str) -> Result<u64> {
    u64::try_from(count).with_context(|| format!("cache layout {label} count exceeds u64"))
}

fn is_sha256(value: &str) -> bool {
    value.len() == "sha256:".len() + 64
        && value.starts_with("sha256:")
        && value["sha256:".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "tests/cache_layout_tests.rs"]
mod tests;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{CompletionRequest, MessageRole};

/// Schema version for the provider-neutral, hash-only request cache layout proof.
pub const CACHE_LAYOUT_PROOF_SCHEMA_VERSION: u16 = 1;

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
#[serde(rename_all = "snake_case", deny_unknown_fields)]
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
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CacheLayoutMutationProofV1 {
    pub kind: CacheLayoutMutationKind,
    /// Number of earlier conversation messages proven byte-identical at the logical layer.
    pub reusable_conversation_message_count: u64,
    /// Whether the local stable prefix bytes are unchanged. This does not claim a provider hit.
    pub local_stable_prefix_preserved: bool,
}

/// Hash-only proof of the provider-neutral request shape frozen before provider I/O.
///
/// Hashes use deterministic, domain-separated SHA-256 so they remain comparable after restart.
/// Raw messages, tool schemas, paths, continuation payloads and partition keys are never stored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
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
        Ok(())
    }
}

#[derive(Serialize)]
struct ToolSchemaMaterialV1<'a> {
    local_tools: &'a [crate::ToolSpec],
    hosted_tools: &'a [crate::HostedToolRequest],
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

#[allow(clippy::too_many_arguments)]
fn mutation_from_previous(
    previous: Option<&CacheLayoutProofV1>,
    route_hash: &str,
    system_hash: &str,
    tool_schema_hash: &str,
    conversation_hash: &str,
    dynamic_state_hash: &str,
    conversation_messages: &[crate::ModelMessage],
) -> Result<CacheLayoutMutationProofV1> {
    let Some(previous) = previous else {
        return Ok(CacheLayoutMutationProofV1 {
            kind: CacheLayoutMutationKind::FirstObservation,
            reusable_conversation_message_count: 0,
            local_stable_prefix_preserved: false,
        });
    };
    previous.validate()?;
    let previous_conversation_count = usize::try_from(previous.conversation_message_count)
        .context("prior cache layout conversation count exceeds usize")?;
    let reusable_conversation_message_count = if previous_conversation_count
        <= conversation_messages.len()
        && hash_canonical(
            "sigil-cache-layout-conversation-v1",
            &conversation_messages[..previous_conversation_count],
        )? == previous.conversation_hash
    {
        previous.conversation_message_count
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
    })
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

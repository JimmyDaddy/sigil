use std::{fmt, path::Path, sync::OnceLock};

use anyhow::{Context, Result, bail};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::provider::CompletionRequest;

/// Schema version for the provider-neutral request material frozen before provider I/O.
pub const PROVIDER_REQUEST_MATERIAL_SCHEMA_VERSION: u16 = 2;
pub const PROVIDER_REQUEST_ENVELOPE_SCHEMA_VERSION: u16 = 1;

static PROVIDER_REQUEST_MATERIAL_FINGERPRINT_KEY: OnceLock<[u8; 32]> = OnceLock::new();

/// Durable frontier that closed the provider-visible source surface before dispatch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderRequestSourceFrontierV1 {
    pub session_id: String,
    pub durable_end_offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_checksum: Option<String>,
}

impl From<&crate::ActiveProjectionFrontier> for ProviderRequestSourceFrontierV1 {
    fn from(frontier: &crate::ActiveProjectionFrontier) -> Self {
        let cursor = frontier.cursor();
        Self {
            session_id: frontier.session_id().to_owned(),
            durable_end_offset: frontier.durable_end_offset(),
            stream_sequence: cursor.map(|cursor| cursor.last_applied_stream_sequence),
            event_id: cursor.map(|cursor| cursor.last_applied_event_id.clone()),
            record_checksum: cursor.map(|cursor| cursor.last_applied_record_checksum.clone()),
        }
    }
}

/// Honest reconstruction strength for one durable provider request envelope.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderRequestReconstructionDispositionV1 {
    /// No durable session frontier exists; only the process-local frozen material is exact.
    InMemoryOnly,
    /// A durable frontier plus the same stable runtime/configuration inputs can rebuild the request.
    DurableFrontierAndRuntimeInputs,
    /// Exact process-local material contains an overlay or opaque carrier not recoverable from the
    /// durable session and stable runtime inputs alone.
    ProcessLocalOverlayRequired,
}

/// Bounded reason why exact provider material cannot be rebuilt from durable safe inputs alone.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderRequestNonReconstructableReasonV1 {
    InMemorySession,
    ExactMessageOverlay,
    PreviousResponseHandle,
    ProviderContinuationState,
    ResolvedImageAttachment,
}

/// Privacy-safe durable proof for one exact provider-neutral request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderRequestEnvelopeV1 {
    pub schema_version: u16,
    pub provider_name: String,
    pub model_name: String,
    pub canonical_request_hash: String,
    pub process_local_material_fingerprint: String,
    pub cache_layout_hash: String,
    pub route_hash: String,
    pub system_hash: String,
    pub tool_schema_hash: String,
    pub conversation_hash: String,
    pub dynamic_state_hash: String,
    pub message_count: u64,
    pub source_message_id_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_frontier: Option<ProviderRequestSourceFrontierV1>,
    pub context_epoch_id: String,
    pub reconstruction_disposition: ProviderRequestReconstructionDispositionV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub non_reconstructable_reasons: Vec<ProviderRequestNonReconstructableReasonV1>,
}

impl ProviderRequestEnvelopeV1 {
    /// Verifies that process-local frozen material is the exact request bound to this envelope.
    ///
    /// The keyed fingerprint includes the process-local secret and session scope, so this proof is
    /// intentionally valid only while the current process retains the same material authority. It
    /// must not be persisted or substituted for durable-frontier reconstruction after restart.
    pub(crate) fn verify_exact_process_local_request(
        &self,
        material: &FrozenProviderRequestMaterial,
    ) -> Result<()> {
        self.validate()?;
        if self.process_local_material_fingerprint != material.fingerprint() {
            bail!("provider request process-local material fingerprint mismatch");
        }
        self.verify_reconstructed_request(material.request())
    }

    /// Verifies a rebuilt provider-neutral request without requiring the original process HMAC key.
    ///
    /// This proves canonical request equality and cache-layout component equality. The caller still
    /// owns proving that the runtime inputs were loaded from the envelope's durable frontier.
    pub fn verify_reconstructed_request(&self, request: &CompletionRequest) -> Result<()> {
        self.validate()?;
        validate_request_shape(
            self.source_frontier
                .as_ref()
                .map_or("in-memory-envelope", |frontier| {
                    frontier.session_id.as_str()
                }),
            request,
        )?;
        if request.provider_name != self.provider_name || request.model_name != self.model_name {
            bail!("provider request reconstruction route does not match its envelope");
        }
        if canonical_request_hash(request)? != self.canonical_request_hash {
            bail!("provider request reconstruction canonical hash mismatch");
        }
        let layout = crate::CacheLayoutProofV1::from_request(request, None)?;
        if layout.layout_hash != self.cache_layout_hash
            || layout.route_hash != self.route_hash
            || layout.system_hash != self.system_hash
            || layout.tool_schema_hash != self.tool_schema_hash
            || layout.conversation_hash != self.conversation_hash
            || layout.dynamic_state_hash != self.dynamic_state_hash
        {
            bail!("provider request reconstruction cache layout mismatch");
        }
        let message_count = u64::try_from(request.messages.len())
            .context("provider request message count exceeds u64")?;
        if message_count != self.message_count
            || source_message_id_hash(request)? != self.source_message_id_hash
        {
            bail!("provider request reconstruction message source summary mismatch");
        }
        Ok(())
    }

    /// Verifies a rebuilt request against both this envelope and its exact durable source prefix.
    ///
    /// The caller supplies the stable runtime/configuration inputs used by the normal request
    /// builder. This method fails closed for envelopes that require process-local overlays, then
    /// proves that the named JSONL byte offset is a real event boundary whose terminal durable
    /// identity matches the envelope before checking exact canonical request equality.
    pub fn verify_reconstructed_request_at_frontier(
        &self,
        session_path: impl AsRef<Path>,
        request: &CompletionRequest,
    ) -> Result<ProviderRequestReconstructionProofV1> {
        self.validate()?;
        if self.reconstruction_disposition
            != ProviderRequestReconstructionDispositionV1::DurableFrontierAndRuntimeInputs
        {
            bail!("provider request envelope is not reconstructable from durable safe inputs");
        }
        let frontier = self
            .source_frontier
            .as_ref()
            .context("reconstructable provider request envelope has no source frontier")?;
        let records = crate::JsonlSessionStore::read_event_records_through_provider_frontier(
            session_path,
            frontier,
        )?;
        self.verify_reconstructed_request(request)?;
        Ok(ProviderRequestReconstructionProofV1 {
            schema_version: PROVIDER_REQUEST_RECONSTRUCTION_PROOF_SCHEMA_VERSION,
            session_id: frontier.session_id.clone(),
            durable_end_offset: frontier.durable_end_offset,
            stream_sequence: frontier.stream_sequence,
            canonical_request_hash: self.canonical_request_hash.clone(),
            source_record_count: u64::try_from(records.len())
                .context("provider request source record count exceeds u64")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.schema_version != PROVIDER_REQUEST_ENVELOPE_SCHEMA_VERSION {
            bail!(
                "unsupported provider request envelope schema version {}",
                self.schema_version
            );
        }
        if !is_bounded_identity(&self.provider_name, 256)
            || !is_bounded_identity(&self.model_name, 256)
            || !is_bounded_identity(&self.context_epoch_id, 512)
        {
            bail!("provider request envelope identity is incomplete");
        }
        for (label, hash) in [
            ("canonical request", &self.canonical_request_hash),
            ("cache layout", &self.cache_layout_hash),
            ("route", &self.route_hash),
            ("system", &self.system_hash),
            ("tool schema", &self.tool_schema_hash),
            ("conversation", &self.conversation_hash),
            ("dynamic state", &self.dynamic_state_hash),
            ("source message id", &self.source_message_id_hash),
        ] {
            if !is_sha256(hash) {
                bail!("provider request envelope {label} hash is malformed");
            }
        }
        if !is_hmac_sha256(&self.process_local_material_fingerprint) {
            bail!("provider request envelope process-local fingerprint is malformed");
        }
        if self.source_frontier.is_none()
            != matches!(
                self.reconstruction_disposition,
                ProviderRequestReconstructionDispositionV1::InMemoryOnly
            )
        {
            bail!("provider request envelope frontier/disposition is inconsistent");
        }
        if let Some(frontier) = &self.source_frontier
            && (!is_bounded_identity(&frontier.session_id, 512)
                || frontier.event_id.is_some() != frontier.stream_sequence.is_some()
                || frontier.record_checksum.is_some() != frontier.stream_sequence.is_some()
                || frontier
                    .event_id
                    .as_deref()
                    .is_some_and(|event_id| !is_bounded_identity(event_id, 512))
                || frontier
                    .record_checksum
                    .as_deref()
                    .is_some_and(|checksum| !is_bounded_identity(checksum, 512)))
        {
            bail!("provider request envelope source frontier is malformed");
        }
        Ok(())
    }
}

pub const PROVIDER_REQUEST_RECONSTRUCTION_PROOF_SCHEMA_VERSION: u16 = 1;

/// Privacy-safe evidence that exact durable source and exact rebuilt request were jointly proven.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderRequestReconstructionProofV1 {
    pub schema_version: u16,
    pub session_id: String,
    pub durable_end_offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_sequence: Option<u64>,
    pub canonical_request_hash: String,
    pub source_record_count: u64,
}

/// Immutable provider-neutral request material captured before a provider call.
///
/// The canonical bytes can contain user content and other sensitive request fields. They are
/// process-local only: callers must not persist or log them. The fingerprint is a process-keyed,
/// session-bound integrity tag suitable for durable audit references within the current run.
#[derive(Clone)]
pub struct FrozenProviderRequestMaterial {
    session_scope_id: String,
    request: CompletionRequest,
    canonical_bytes: Vec<u8>,
    fingerprint: String,
}

impl fmt::Debug for FrozenProviderRequestMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FrozenProviderRequestMaterial")
            .field("schema_version", &PROVIDER_REQUEST_MATERIAL_SCHEMA_VERSION)
            .field("fingerprint", &self.fingerprint)
            .field("canonical_byte_size", &self.canonical_bytes.len())
            .finish()
    }
}

impl FrozenProviderRequestMaterial {
    /// Freezes one complete provider-neutral request and derives its safe audit fingerprint.
    ///
    /// # Errors
    ///
    /// Returns an error when the session scope is missing, request identity is incomplete, or
    /// canonical materialization cannot represent the provider-neutral request exactly.
    pub fn freeze(session_scope_id: &str, request: CompletionRequest) -> Result<Self> {
        validate_request_shape(session_scope_id, &request)?;
        let canonical_bytes = canonical_request_bytes(&request)?;
        let fingerprint = request_material_fingerprint(session_scope_id, &canonical_bytes)?;
        Ok(Self {
            session_scope_id: session_scope_id.to_owned(),
            request,
            canonical_bytes,
            fingerprint,
        })
    }

    /// Returns the immutable provider request that was represented by this material.
    #[must_use]
    pub fn request(&self) -> &CompletionRequest {
        &self.request
    }

    /// Returns the durable session scope that was bound into this material fingerprint.
    #[must_use]
    pub fn session_scope_id(&self) -> &str {
        &self.session_scope_id
    }

    /// Returns the process-keyed, session-bound audit fingerprint.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Returns the exact canonical bytes for in-process proof or provider materialization only.
    ///
    /// These bytes can contain sensitive request content and must never be persisted or logged.
    #[must_use]
    pub fn canonical_bytes_for_in_process_use(&self) -> &[u8] {
        &self.canonical_bytes
    }

    /// Builds a deterministic, hash-only cache shape proof for this frozen request.
    ///
    /// The proof is independent from the process-keyed request fingerprint and is therefore
    /// suitable for cross-restart comparison. It never contains raw request material.
    ///
    /// # Errors
    ///
    /// Returns an error when the request subsets cannot be canonicalized or the previous proof is
    /// malformed.
    pub fn cache_layout_proof(
        &self,
        previous: Option<&crate::CacheLayoutProofV1>,
    ) -> Result<crate::CacheLayoutProofV1> {
        crate::CacheLayoutProofV1::from_request(&self.request, previous)
    }

    /// Builds the V2 semantic cache layout proof without ephemeral hosted authorization fields.
    pub fn semantic_cache_layout_proof_v2(
        &self,
        previous: Option<&crate::CacheLayoutProofV2>,
    ) -> Result<crate::CacheLayoutProofV2> {
        crate::CacheLayoutProofV2::from_request(&self.request, previous)
    }

    /// Builds the privacy-safe durable envelope bound to this exact in-process material.
    pub fn request_envelope(
        &self,
        cache_layout: &crate::CacheLayoutProofV1,
        source_frontier: Option<ProviderRequestSourceFrontierV1>,
        context_epoch_id: impl Into<String>,
    ) -> Result<ProviderRequestEnvelopeV1> {
        self.request_envelope_with_durable_runtime_state(
            cache_layout,
            source_frontier,
            context_epoch_id,
            None,
            &[],
        )
    }

    /// Builds an envelope while proving opaque request carriers against the exact durable state
    /// visible at the source frontier.
    ///
    /// The ordinary public builder remains fail-closed because an arbitrary frozen request cannot
    /// prove where its opaque carriers came from. Provider-attempt construction owns the live
    /// session and may use this stronger path only after comparing the exact values with that
    /// session's append-only control projection.
    pub(crate) fn request_envelope_with_durable_runtime_state(
        &self,
        cache_layout: &crate::CacheLayoutProofV1,
        source_frontier: Option<ProviderRequestSourceFrontierV1>,
        context_epoch_id: impl Into<String>,
        durable_previous_response_handle: Option<&crate::ResponseHandle>,
        durable_continuation_states: &[crate::ProviderContinuationState],
    ) -> Result<ProviderRequestEnvelopeV1> {
        cache_layout.validate()?;
        let mut reasons = std::collections::BTreeSet::new();
        if source_frontier.is_none() {
            reasons.insert(ProviderRequestNonReconstructableReasonV1::InMemorySession);
        }
        if !self.request.deterministic_materialization {
            reasons.insert(ProviderRequestNonReconstructableReasonV1::ExactMessageOverlay);
        }
        if self.request.previous_response_handle.as_ref() != durable_previous_response_handle
            && self.request.previous_response_handle.is_some()
        {
            reasons.insert(ProviderRequestNonReconstructableReasonV1::PreviousResponseHandle);
        }
        if self.request.continuation_states.as_slice() != durable_continuation_states
            && !self.request.continuation_states.is_empty()
        {
            reasons.insert(ProviderRequestNonReconstructableReasonV1::ProviderContinuationState);
        }
        if self
            .request
            .messages
            .iter()
            .any(|message| !message.image_attachments.is_empty())
        {
            reasons.insert(ProviderRequestNonReconstructableReasonV1::ResolvedImageAttachment);
        }
        let reconstruction_disposition = if source_frontier.is_none() {
            ProviderRequestReconstructionDispositionV1::InMemoryOnly
        } else if reasons.is_empty() {
            ProviderRequestReconstructionDispositionV1::DurableFrontierAndRuntimeInputs
        } else {
            ProviderRequestReconstructionDispositionV1::ProcessLocalOverlayRequired
        };
        let envelope = ProviderRequestEnvelopeV1 {
            schema_version: PROVIDER_REQUEST_ENVELOPE_SCHEMA_VERSION,
            provider_name: self.request.provider_name.clone(),
            model_name: self.request.model_name.clone(),
            canonical_request_hash: canonical_request_hash(&self.request)?,
            process_local_material_fingerprint: self.fingerprint.clone(),
            cache_layout_hash: cache_layout.layout_hash.clone(),
            route_hash: cache_layout.route_hash.clone(),
            system_hash: cache_layout.system_hash.clone(),
            tool_schema_hash: cache_layout.tool_schema_hash.clone(),
            conversation_hash: cache_layout.conversation_hash.clone(),
            dynamic_state_hash: cache_layout.dynamic_state_hash.clone(),
            message_count: u64::try_from(self.request.messages.len())
                .context("provider request message count exceeds u64")?,
            source_message_id_hash: source_message_id_hash(&self.request)?,
            source_frontier,
            context_epoch_id: context_epoch_id.into(),
            reconstruction_disposition,
            non_reconstructable_reasons: reasons.into_iter().collect(),
        };
        envelope.validate()?;
        Ok(envelope)
    }

    /// Consumes the frozen material and yields the exact request that was frozen.
    #[must_use]
    pub fn into_request(self) -> CompletionRequest {
        self.request
    }
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
struct ProviderRequestMaterialV1<'a> {
    // Keep this explicit representation in lockstep with `CompletionRequest`: request serde
    // defaults must not silently omit a field from the pre-send material fingerprint.
    schema_version: u16,
    provider_name: &'a str,
    model_name: &'a str,
    messages: &'a [crate::ModelMessage],
    tools: &'a [crate::ToolSpec],
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    reasoning_effort: Option<&'a crate::ReasoningEffort>,
    previous_response_handle: Option<&'a crate::ResponseHandle>,
    continuation_states: &'a [crate::ProviderContinuationState],
    traffic_partition_key: Option<&'a str>,
    background: bool,
    store: bool,
    deterministic_materialization: bool,
    hosted_tools: &'a [crate::HostedToolRequest],
}

fn validate_request_shape(session_scope_id: &str, request: &CompletionRequest) -> Result<()> {
    if session_scope_id.trim().is_empty() {
        bail!("provider request material requires a non-empty session scope id");
    }
    if request.provider_name.trim().is_empty() {
        bail!("provider request material requires a non-empty provider name");
    }
    if request.model_name.trim().is_empty() {
        bail!("provider request material requires a non-empty model name");
    }
    if request
        .temperature
        .is_some_and(|temperature| !temperature.is_finite())
    {
        bail!("provider request material does not support a non-finite temperature");
    }
    Ok(())
}

fn canonical_request_bytes(request: &CompletionRequest) -> Result<Vec<u8>> {
    let material = ProviderRequestMaterialV1 {
        schema_version: PROVIDER_REQUEST_MATERIAL_SCHEMA_VERSION,
        provider_name: &request.provider_name,
        model_name: &request.model_name,
        messages: &request.messages,
        tools: &request.tools,
        temperature: request.temperature,
        max_tokens: request.max_tokens,
        reasoning_effort: request.reasoning_effort.as_ref(),
        previous_response_handle: request.previous_response_handle.as_ref(),
        continuation_states: &request.continuation_states,
        traffic_partition_key: request.traffic_partition_key.as_deref(),
        background: request.background,
        store: request.store,
        deterministic_materialization: request.deterministic_materialization,
        hosted_tools: &request.hosted_tools,
    };
    let value =
        serde_json::to_value(material).context("failed to serialize provider request material")?;
    crate::event::canonical_json_bytes(&value)
}

fn canonical_request_hash(request: &CompletionRequest) -> Result<String> {
    hash_bytes(
        "sigil-provider-request-envelope-canonical-v1",
        &canonical_request_bytes(request)?,
    )
}

fn source_message_id_hash(request: &CompletionRequest) -> Result<String> {
    let ids = request
        .messages
        .iter()
        .map(|message| message.id.as_str())
        .collect::<Vec<_>>();
    let bytes = crate::event::canonical_json_bytes(&serde_json::to_value(ids)?)?;
    hash_bytes("sigil-provider-request-envelope-message-ids-v1", &bytes)
}

fn hash_bytes(domain: &str, bytes: &[u8]) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update((domain.len() as u64).to_be_bytes());
    digest.update(domain.as_bytes());
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn is_sha256(value: &str) -> bool {
    value.len() == "sha256:".len() + 64
        && value.starts_with("sha256:")
        && value["sha256:".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
}

fn is_hmac_sha256(value: &str) -> bool {
    value.len() == "hmac-sha256:".len() + 64
        && value.starts_with("hmac-sha256:")
        && value["hmac-sha256:".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
}

fn is_bounded_identity(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn request_material_fingerprint(session_scope_id: &str, canonical_bytes: &[u8]) -> Result<String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(provider_request_material_fingerprint_key())
        .context("failed to initialize provider request material fingerprint")?;
    update_fingerprint_part(
        &mut mac,
        "schema_version",
        &PROVIDER_REQUEST_MATERIAL_SCHEMA_VERSION.to_string(),
    );
    update_fingerprint_part(&mut mac, "session_scope_id", session_scope_id);
    update_fingerprint_bytes(&mut mac, "canonical_request", canonical_bytes);
    Ok(format!("hmac-sha256:{:x}", mac.finalize().into_bytes()))
}

fn provider_request_material_fingerprint_key() -> &'static [u8; 32] {
    PROVIDER_REQUEST_MATERIAL_FINGERPRINT_KEY.get_or_init(|| {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut key = [0_u8; 32];
        key[..16].copy_from_slice(first.as_bytes());
        key[16..].copy_from_slice(second.as_bytes());
        key
    })
}

fn update_fingerprint_part(mac: &mut Hmac<Sha256>, label: &str, value: &str) {
    update_fingerprint_bytes(mac, label, value.as_bytes());
}

fn update_fingerprint_bytes(mac: &mut Hmac<Sha256>, label: &str, value: &[u8]) {
    mac.update(&(label.len() as u64).to_be_bytes());
    mac.update(label.as_bytes());
    mac.update(&(value.len() as u64).to_be_bytes());
    mac.update(value);
}

#[cfg(test)]
#[path = "tests/provider_request_material_tests.rs"]
mod tests;

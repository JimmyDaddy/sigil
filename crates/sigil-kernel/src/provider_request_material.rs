use std::{fmt, sync::OnceLock};

use anyhow::{Context, Result, bail};
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use uuid::Uuid;

use crate::provider::CompletionRequest;

/// Schema version for the provider-neutral request material frozen before provider I/O.
pub const PROVIDER_REQUEST_MATERIAL_SCHEMA_VERSION: u16 = 2;

static PROVIDER_REQUEST_MATERIAL_FINGERPRINT_KEY: OnceLock<[u8; 32]> = OnceLock::new();

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

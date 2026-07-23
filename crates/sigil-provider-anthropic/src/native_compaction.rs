use anyhow::{Context, Result, bail};
use reqwest::header::RETRY_AFTER;
use serde_json::{Value, json, value::RawValue};
use sigil_kernel::{
    CompactionCursor, CompletionRequest, ContextSensitivity, FrozenProviderRequestMaterial,
    NativeProviderCompactionAttempt, NativeProviderCompactionMaterialization,
    NativeProviderCompactionMetadata, NativeProviderCompactionRequest, Provider,
    ProviderPhysicalAttemptOutcome, ProviderStreamTimeoutState, Session, VersionedProfileIdentity,
    provider_continuation_route_fingerprint, provider_status_error, timeout_provider_request,
    timeout_provider_stream_next,
};

use crate::{
    AnthropicProvider,
    hosted_search::{AnthropicHostedContinuationStore, AnthropicHostedPlatform},
    provider::{provider_timeout_error, read_error_response_body},
    request::build_messages_request_with_continuations,
};

/// Anthropic's documented beta header for Messages context compaction.
pub const ANTHROPIC_NATIVE_COMPACTION_BETA_HEADER: &str = "compact-2026-01-12";
/// The lowest `input_tokens` trigger accepted by Anthropic's public compaction beta.
pub const ANTHROPIC_NATIVE_COMPACTION_MIN_TRIGGER_TOKENS: u64 = 50_000;

const ANTHROPIC_NATIVE_COMPACTION_RESPONSE_LIMIT_BYTES: usize = 64 * 1024 * 1024;

/// Provider-local controls for one explicitly invoked, paused Anthropic compaction pass.
///
/// This is intentionally not part of normal provider configuration: K25.13 only records an
/// encrypted candidate. It neither enables a TUI action nor changes the active context boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicNativeCompactionOptions {
    /// Input-token threshold at which Anthropic is allowed to produce its compaction block.
    pub trigger_input_tokens: u64,
    /// Optional provider-native replacement for Anthropic's default compaction instructions.
    pub instructions: Option<String>,
}

impl AnthropicNativeCompactionOptions {
    fn validate(&self) -> Result<()> {
        if self.trigger_input_tokens < ANTHROPIC_NATIVE_COMPACTION_MIN_TRIGGER_TOKENS {
            bail!(
                "Anthropic native compaction trigger must be at least {ANTHROPIC_NATIVE_COMPACTION_MIN_TRIGGER_TOKENS} input tokens"
            );
        }
        if self
            .instructions
            .as_deref()
            .is_some_and(|instructions| instructions.trim().is_empty())
        {
            bail!("Anthropic native compaction instructions must not be empty");
        }
        Ok(())
    }
}

/// One paused Messages response, retaining the provider-native replacement content verbatim.
pub struct AnthropicPausedCompactionResponse {
    /// Anthropic's message identifier for the physical request.
    pub response_id: String,
    compacted_content: Option<Box<RawValue>>,
}

impl std::fmt::Debug for AnthropicPausedCompactionResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicPausedCompactionResponse")
            .field("response_id", &self.response_id)
            .field("has_compacted_content", &self.compacted_content.is_some())
            .finish()
    }
}

impl AnthropicPausedCompactionResponse {
    /// Returns the complete `content` array exactly as received when compaction actually paused.
    #[must_use]
    pub fn canonical_compacted_content_json(&self) -> Option<&str> {
        self.compacted_content.as_deref().map(RawValue::get)
    }
}

impl AnthropicProvider {
    /// Calls Anthropic's public paused Messages-compaction beta once.
    ///
    /// The call is deliberately non-streaming and sets `pause_after_compaction=true`. A paused
    /// response must consist of exactly one non-empty provider-native `compaction` block, which
    /// this method retains as raw JSON. A normal response means the threshold did not trigger and
    /// returns no replacement window. There is no transparent retry because the remote service
    /// may already have compacted the window.
    pub async fn compact(
        &self,
        request: &CompletionRequest,
        options: &AnthropicNativeCompactionOptions,
    ) -> Result<AnthropicPausedCompactionResponse> {
        self.ensure_native_compaction_supported(request, options)?;
        let body = build_paused_compaction_request(request, self.config.max_tokens, options)?;
        let response = timeout_provider_request(
            self.post_json_with_required_beta(
                &self.messages_url(),
                &body,
                &[ANTHROPIC_NATIVE_COMPACTION_BETA_HEADER],
            ),
            self.timeouts,
        )
        .await
        .map_err(|phase| {
            provider_timeout_error(phase, self.timeouts, "anthropic", &request.model_name)
        })?
        .context("Anthropic native compaction request failed")?;
        let status = response.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let error_body = read_error_response_body(
                response,
                self.timeouts.request_timeout,
                &sigil_kernel::SecretRedactor::from_values([self.api_key()?]),
                "anthropic",
                &request.model_name,
                status_code,
            )
            .await?;
            return Err(provider_status_error(
                status_code,
                retry_after.as_deref(),
                crate::errors::classify_status(status_code, error_body.text()).into(),
            ));
        }
        let payload =
            read_paused_compaction_response_body(response, self.timeouts, &request.model_name)
                .await?;
        parse_paused_compaction_response(&payload)
    }

    /// Sends one frozen Anthropic Messages window through the paused compaction beta and records
    /// a provider-neutral encrypted K25.12 candidate if the provider actually compacts it.
    ///
    /// This internal driver never activates a candidate, removes history, or enables ordinary
    /// TUI/automatic compaction. A normal no-compaction response is a completed physical attempt
    /// with no durable candidate, rather than a guessed replacement window.
    pub async fn compact_and_materialize_durable(
        &self,
        session: &Session,
        logical_run_id: impl Into<String>,
        frozen_request: FrozenProviderRequestMaterial,
        covers_through: CompactionCursor,
        options: AnthropicNativeCompactionOptions,
    ) -> Result<Option<NativeProviderCompactionMaterialization>> {
        let request = frozen_request.request();
        self.ensure_native_compaction_supported(request, &options)?;
        let metadata =
            self.native_compaction_metadata(session.session_scope_id(), &request.model_name)?;
        let mut attempt = NativeProviderCompactionAttempt::start(
            session,
            NativeProviderCompactionRequest {
                logical_run_id: logical_run_id.into(),
                frozen_request,
                covers_through,
                metadata,
            },
        )
        .await?;
        let compacted = match self.compact(attempt.request().request(), &options).await {
            Ok(compacted) => compacted,
            Err(error) => {
                if let Err(terminal_error) = attempt
                    .finish(
                        ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain,
                        None,
                    )
                    .await
                {
                    return Err(terminal_error.context(format!(
                        "Anthropic native compaction request failed after its durable start: {error:#}"
                    )));
                }
                return Err(error);
            }
        };
        let response_id = compacted.response_id.clone();
        let Some(raw_content) = compacted.canonical_compacted_content_json() else {
            attempt
                .finish(ProviderPhysicalAttemptOutcome::Completed, Some(response_id))
                .await?;
            return Ok(None);
        };
        let materialized = match attempt
            .materialize_artifact(response_id.clone(), raw_content.as_bytes().to_vec())
            .await
        {
            Ok(materialized) => materialized,
            Err(error) => {
                if let Err(terminal_error) = attempt
                    .finish(
                        ProviderPhysicalAttemptOutcome::FailedAfterOutputOrSideEffect,
                        Some(response_id),
                    )
                    .await
                {
                    return Err(terminal_error.context(format!(
                        "Anthropic native compaction output materialization failed: {error:#}"
                    )));
                }
                return Err(error);
            }
        };
        attempt
            .finish(ProviderPhysicalAttemptOutcome::Completed, Some(response_id))
            .await?;
        Ok(Some(materialized))
    }

    fn ensure_native_compaction_supported(
        &self,
        request: &CompletionRequest,
        options: &AnthropicNativeCompactionOptions,
    ) -> Result<()> {
        if request.provider_name != self.name() {
            bail!(
                "Anthropic native compaction request belongs to provider {}",
                request.provider_name
            );
        }
        if self.hosted_platform != AnthropicHostedPlatform::ClaudeApi {
            bail!("Anthropic native compaction only supports the official Anthropic API base URL");
        }
        if !is_native_compaction_model(&request.model_name) {
            bail!(
                "Anthropic native compaction does not support model {}",
                request.model_name
            );
        }
        if !request.tools.is_empty() || !request.hosted_tools.is_empty() {
            bail!("Anthropic native compaction currently requires a frozen window without tools");
        }
        if !request.continuation_states.is_empty() {
            bail!(
                "Anthropic native compaction currently requires a frozen window without provider continuation state"
            );
        }
        options.validate()
    }

    fn native_compaction_metadata(
        &self,
        session_scope_id: &str,
        model_name: &str,
    ) -> Result<NativeProviderCompactionMetadata> {
        Ok(NativeProviderCompactionMetadata {
            provider_route_fingerprint: provider_continuation_route_fingerprint(
                session_scope_id,
                self.name(),
                &self.config.base_url,
            )?,
            model_metadata_profile: VersionedProfileIdentity::from_content(
                "anthropic-messages-compaction-model-metadata",
                1,
                format!("provider=anthropic;model={model_name};beta={ANTHROPIC_NATIVE_COMPACTION_BETA_HEADER}")
                    .as_bytes(),
            ),
            wire_profile: VersionedProfileIdentity::from_content(
                "anthropic-messages-paused-compaction-wire",
                1,
                b"POST /v1/messages;anthropic-beta=compact-2026-01-12;context_management.edits=compact_20260112;pause_after_compaction=true",
            ),
            wire_protocol: "anthropic_messages".to_owned(),
            wire_schema_version: "compact_20260112".to_owned(),
            composition_profile: VersionedProfileIdentity::from_content(
                "anthropic-messages-paused-compaction-content",
                1,
                b"next Messages request uses the paused assistant content array unchanged",
            ),
            artifact_kind: "anthropic_messages_compaction_content".to_owned(),
            sensitivity: ContextSensitivity::Repository,
        })
    }
}

pub(crate) fn build_paused_compaction_request(
    request: &CompletionRequest,
    default_max_tokens: u32,
    options: &AnthropicNativeCompactionOptions,
) -> Result<crate::models::AnthropicMessagesRequest> {
    options.validate()?;
    if !request.tools.is_empty() || !request.hosted_tools.is_empty() {
        bail!("Anthropic native compaction currently requires a frozen window without tools");
    }
    if !request.continuation_states.is_empty() {
        bail!(
            "Anthropic native compaction currently requires a frozen window without provider continuation state"
        );
    }
    let mut compactable = request.clone();
    sigil_kernel::strip_request_image_attachments_for_compaction(&mut compactable);
    let mut body = build_messages_request_with_continuations(
        &compactable,
        default_max_tokens,
        &AnthropicHostedContinuationStore::default(),
    )?
    .body;
    body.stream = false;
    let mut edit = serde_json::Map::new();
    edit.insert(
        "type".to_owned(),
        Value::String("compact_20260112".to_owned()),
    );
    edit.insert(
        "trigger".to_owned(),
        json!({"type": "input_tokens", "value": options.trigger_input_tokens}),
    );
    edit.insert("pause_after_compaction".to_owned(), Value::Bool(true));
    if let Some(instructions) = &options.instructions {
        edit.insert(
            "instructions".to_owned(),
            Value::String(instructions.clone()),
        );
    }
    body.context_management = Some(json!({"edits": [Value::Object(edit)]}));
    Ok(body)
}

async fn read_paused_compaction_response_body(
    response: reqwest::Response,
    timeouts: sigil_kernel::ModelRequestTimeouts,
    model_name: &str,
) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut timeout_state = ProviderStreamTimeoutState::new(timeouts);
    let mut payload = Vec::new();
    loop {
        match timeout_provider_stream_next(&mut stream, timeouts, &mut timeout_state).await {
            Ok(Some(Ok(bytes))) => {
                let next_len = payload.len().saturating_add(bytes.len());
                if next_len > ANTHROPIC_NATIVE_COMPACTION_RESPONSE_LIMIT_BYTES {
                    bail!(
                        "Anthropic native compaction response exceeds {ANTHROPIC_NATIVE_COMPACTION_RESPONSE_LIMIT_BYTES} bytes"
                    );
                }
                payload.extend_from_slice(&bytes);
            }
            Ok(Some(Err(error))) => {
                return Err(error).context("failed to read Anthropic native compaction response");
            }
            Err(phase) => {
                return Err(provider_timeout_error(
                    phase,
                    timeouts,
                    "anthropic",
                    model_name,
                ));
            }
            Ok(None) => return Ok(payload),
        }
    }
}

fn parse_paused_compaction_response(payload: &[u8]) -> Result<AnthropicPausedCompactionResponse> {
    #[derive(serde::Deserialize)]
    struct PausedCompactionResponse {
        id: String,
        #[serde(default)]
        stop_reason: Option<String>,
        content: Box<RawValue>,
    }

    let response: PausedCompactionResponse = serde_json::from_slice(payload)
        .context("invalid Anthropic native compaction response JSON")?;
    if response.id.trim().is_empty() {
        bail!("Anthropic native compaction response is missing its id");
    }
    if response.stop_reason.as_deref() != Some("compaction") {
        return Ok(AnthropicPausedCompactionResponse {
            response_id: response.id,
            compacted_content: None,
        });
    }
    let content: Vec<Value> = serde_json::from_str(response.content.get())
        .context("Anthropic paused compaction content must be a JSON array")?;
    let [block] = content.as_slice() else {
        bail!(
            "Anthropic paused compaction response must contain exactly one compaction content block"
        );
    };
    if block.get("type").and_then(Value::as_str) != Some("compaction") {
        bail!("Anthropic paused compaction response is missing its compaction content block");
    }
    if block
        .get("content")
        .and_then(Value::as_str)
        .is_none_or(|content| content.trim().is_empty())
    {
        bail!("Anthropic paused compaction content is empty or unavailable");
    }
    Ok(AnthropicPausedCompactionResponse {
        response_id: response.id,
        compacted_content: Some(response.content),
    })
}

fn is_native_compaction_model(model_name: &str) -> bool {
    matches!(
        model_name,
        "claude-fable-5"
            | "claude-mythos-5"
            | "claude-mythos-preview"
            | "claude-opus-4-8"
            | "claude-opus-4-7"
            | "claude-opus-4-6"
            | "claude-sonnet-5"
            | "claude-sonnet-4-6"
    )
}

#[cfg(test)]
#[path = "tests/native_compaction_tests.rs"]
mod tests;

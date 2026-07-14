use std::{collections::VecDeque, pin::Pin, time::Duration};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, stream};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, value::RawValue};
use sigil_kernel::{
    COMPACTION_TOKEN_PROOF_SCHEMA_VERSION, CompactionCursor, CompletionRequest, ContextSensitivity,
    EffectiveTokenBudget, FrozenProviderRequestMaterial, InputTokenEvidence, ModelRequestTimeouts,
    NativeProviderCompactionAttempt, NativeProviderCompactionMaterialization,
    NativeProviderCompactionMetadata, NativeProviderCompactionRequest,
    PROVIDER_ERROR_BODY_LIMIT_BYTES, PortableTargetRequestMaterial, Provider, ProviderCapabilities,
    ProviderChunk, ProviderPhysicalAttemptOutcome, ProviderRequestRejection,
    ProviderStreamTimeoutState, ProviderTimeoutMetadata, ProviderTimeoutPhase, RequestFitProof,
    SecretRedactor, Session, TokenMeasurementBinding, TokenMeasurementScope,
    VersionedProfileIdentity, provider_continuation_route_fingerprint, read_provider_error_body,
    timeout_provider_request, timeout_provider_stream_next,
};

use crate::{
    capabilities::openai_responses_capabilities,
    client::build_http_client,
    config::OpenAiResponsesProviderConfig,
    errors::{OpenAiResponsesProviderError, classify_status},
    mapper::StreamMapper,
    models::OpenAiResponsesCompactedWindow,
    request::{build_compaction_request, build_input_token_count_request, build_responses_request},
    stream::{OpenAiResponsesSseDecoder, OpenAiResponsesSseFrame},
};

const OPENAI_RESPONSES_COMPACT_RESPONSE_LIMIT_BYTES: usize = 64 * 1024 * 1024;
const OPENAI_RESPONSES_INPUT_TOKEN_RESPONSE_LIMIT_BYTES: usize = 64 * 1024;

/// Only this pinned official snapshot currently admits the server-count portable target proof.
pub const OPENAI_RESPONSES_PORTABLE_TARGET_MODEL: &str = "gpt-4.1-2025-04-14";

/// The documented context window for [`OPENAI_RESPONSES_PORTABLE_TARGET_MODEL`].
pub const OPENAI_RESPONSES_PORTABLE_TARGET_CONTEXT_WINDOW_TOKENS: u64 = 1_047_576;

/// The documented maximum output and required explicit target reservation for that snapshot.
pub const OPENAI_RESPONSES_PORTABLE_TARGET_OUTPUT_TOKENS: u32 = 32_768;

const OPENAI_RESPONSES_PORTABLE_TARGET_SAFETY_BUFFER_TOKENS: u64 = 8_192;

#[derive(Clone)]
pub struct OpenAiResponsesProvider {
    config: OpenAiResponsesProviderConfig,
    timeouts: ModelRequestTimeouts,
    capabilities: ProviderCapabilities,
    client: reqwest::Client,
}

impl OpenAiResponsesProvider {
    pub fn new(
        config: OpenAiResponsesProviderConfig,
        timeouts: ModelRequestTimeouts,
    ) -> Result<Self> {
        let config = config.resolved()?;
        Ok(Self {
            timeouts,
            client: build_http_client()?,
            capabilities: openai_responses_capabilities(),
            config,
        })
    }

    fn api_key(&self) -> Result<String> {
        if let Some(api_key) = &self.config.api_key
            && !api_key.trim().is_empty()
        {
            return Ok(api_key.clone());
        }
        Err(OpenAiResponsesProviderError::MissingApiKey.into())
    }

    fn responses_url(&self) -> String {
        format!("{}/responses", self.config.base_url.trim_end_matches('/'))
    }

    fn compact_url(&self) -> String {
        format!(
            "{}/responses/compact",
            self.config.base_url.trim_end_matches('/')
        )
    }

    fn input_token_count_url(&self) -> String {
        format!(
            "{}/responses/input_tokens",
            self.config.base_url.trim_end_matches('/')
        )
    }

    fn uses_official_openai_endpoint(&self) -> bool {
        is_official_openai_base_url(&self.config.base_url)
    }

    /// Calls the stateless OpenAI Responses compact endpoint once.
    ///
    /// The request uses the complete canonical Responses window from `request`. A compact pass
    /// may have been accepted even when a later transport failure obscures its result, so this
    /// method never transparently retries. Its returned output items are provider-opaque and are
    /// left for the caller to store through the durable continuation lifecycle.
    pub async fn compact(
        &self,
        request: &CompletionRequest,
    ) -> Result<OpenAiResponsesCompactedWindow> {
        let body = build_compaction_request(request)?;
        let response =
            timeout_provider_request(self.post_json(&self.compact_url(), &body), self.timeouts)
                .await
                .map_err(|phase| {
                    provider_timeout_error(phase, self.timeouts, self.name(), &request.model_name)
                })?
                .context("OpenAI Responses compact request failed")?;
        let status = response.status();
        if !status.is_success() {
            let error_body = read_error_response_body(
                response,
                self.timeouts.request_timeout,
                &SecretRedactor::from_values([self.api_key()?]),
                self.name(),
                &request.model_name,
                status.as_u16(),
            )
            .await;
            return Err(classify_status(status.as_u16(), error_body?.text()).into());
        }

        let payload =
            read_compaction_response_body(response, self.timeouts, &request.model_name).await?;
        parse_compacted_window(&payload)
    }

    /// Sends one exact frozen Responses window to `/responses/compact` and materializes the
    /// complete returned opaque window through the kernel's encrypted continuation lifecycle.
    ///
    /// This is an internal provider driver, not a user action and not a compaction-boundary
    /// activation. Its caller must supply a durable source cursor for the exact frozen window;
    /// K25.12 admission still decides whether a recorded candidate can ever become usable.
    pub async fn compact_and_materialize_durable(
        &self,
        session: &Session,
        logical_run_id: impl Into<String>,
        frozen_request: FrozenProviderRequestMaterial,
        covers_through: CompactionCursor,
    ) -> Result<NativeProviderCompactionMaterialization> {
        let request = frozen_request.request();
        if request.provider_name != self.name() {
            anyhow::bail!(
                "OpenAI Responses native compaction request belongs to provider {}",
                request.provider_name
            );
        }
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
        let compacted = match self.compact(attempt.request().request()).await {
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
                        "OpenAI Responses compact request failed after its durable start: {error:#}"
                    )));
                }
                return Err(error);
            }
        };
        let response_id = compacted.response_id.clone();
        let opaque_window = compacted.canonical_output_json().as_bytes().to_vec();
        let materialized = match attempt
            .materialize_artifact(response_id.clone(), opaque_window)
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
                        "OpenAI Responses compact output materialization failed: {error:#}"
                    )));
                }
                return Err(error);
            }
        };
        attempt
            .finish(ProviderPhysicalAttemptOutcome::Completed, Some(response_id))
            .await?;
        Ok(materialized)
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
                "openai-responses-model-metadata",
                1,
                format!("provider={};model={model_name}", self.name()).as_bytes(),
            ),
            wire_profile: VersionedProfileIdentity::from_content(
                "openai-responses-compact-wire",
                1,
                b"POST /v1/responses/compact:model+input:response.compaction.output",
            ),
            wire_protocol: "openai_responses".to_owned(),
            wire_schema_version: "compact-v1".to_owned(),
            composition_profile: VersionedProfileIdentity::from_content(
                "openai-responses-compaction-output",
                1,
                b"next responses request uses output as canonical input prefix without pruning",
            ),
            artifact_kind: "responses_compaction_output".to_owned(),
            sensitivity: ContextSensitivity::Repository,
        })
    }

    async fn input_token_count(&self, request: &CompletionRequest) -> Result<u64> {
        let body = build_input_token_count_request(request)?;
        let response = timeout_provider_request(
            self.post_json(&self.input_token_count_url(), &body),
            self.timeouts,
        )
        .await
        .map_err(|phase| {
            provider_timeout_error(phase, self.timeouts, self.name(), &request.model_name)
        })?
        .context("OpenAI Responses input-token request failed")?;
        let status = response.status();
        if !status.is_success() {
            let error_body = read_error_response_body(
                response,
                self.timeouts.request_timeout,
                &SecretRedactor::from_values([self.api_key()?]),
                self.name(),
                &request.model_name,
                status.as_u16(),
            )
            .await;
            return Err(classify_status(status.as_u16(), error_body?.text()).into());
        }
        let payload =
            read_input_token_count_response_body(response, self.timeouts, &request.model_name)
                .await?;
        parse_input_token_count(&payload)
    }
}

fn is_official_openai_base_url(base_url: &str) -> bool {
    base_url.trim_end_matches('/') == "https://api.openai.com/v1"
}

#[async_trait]
impl Provider for OpenAiResponsesProvider {
    fn name(&self) -> &str {
        "openai_responses"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    fn classify_pre_generation_rejection(
        &self,
        error: &anyhow::Error,
    ) -> Option<ProviderRequestRejection> {
        (self.uses_official_openai_endpoint()
            && error
                .downcast_ref::<OpenAiResponsesProviderError>()
                .is_some_and(|error| {
                    matches!(error, OpenAiResponsesProviderError::ContextWindowExceeded)
                }))
        .then_some(ProviderRequestRejection::ContextWindowExceeded)
    }

    async fn prove_portable_compaction_target(
        &self,
        frozen_request: FrozenProviderRequestMaterial,
    ) -> Result<PortableTargetRequestMaterial> {
        let request = frozen_request.request();
        if !self.uses_official_openai_endpoint() {
            anyhow::bail!(
                "official OpenAI Responses endpoint is required for server-count target proof"
            );
        }
        if request.provider_name != self.name() {
            anyhow::bail!(
                "OpenAI Responses server-count request belongs to provider {}",
                request.provider_name
            );
        }
        if request.model_name != OPENAI_RESPONSES_PORTABLE_TARGET_MODEL {
            anyhow::bail!(
                "OpenAI Responses server-count target proof is unavailable for model {}",
                request.model_name
            );
        }
        if request.max_tokens != Some(OPENAI_RESPONSES_PORTABLE_TARGET_OUTPUT_TOKENS) {
            anyhow::bail!(
                "OpenAI Responses server-count target proof requires explicit max_tokens={OPENAI_RESPONSES_PORTABLE_TARGET_OUTPUT_TOKENS}"
            );
        }

        let tokens = self.input_token_count(request).await?;
        let binding = openai_responses_server_count_binding(&request.model_name);
        let proof = RequestFitProof {
            schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            input: InputTokenEvidence::Exact {
                tokens,
                material_fingerprint: frozen_request.fingerprint().to_owned(),
                measurement_scope: TokenMeasurementScope::RenderedTargetInput,
                binding: binding.clone(),
                provider_model_snapshot: Some(OPENAI_RESPONSES_PORTABLE_TARGET_MODEL.to_owned()),
                provider_system_fingerprint: None,
            },
            budget: EffectiveTokenBudget {
                schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
                budget_profile: VersionedProfileIdentity::from_content(
                    "openai-responses-gpt-4.1-portable-target-budget",
                    1,
                    b"model=gpt-4.1-2025-04-14;context_window=1047576;max_output_tokens=32768;safety_buffer_tokens=8192",
                ),
                context_window_tokens: OPENAI_RESPONSES_PORTABLE_TARGET_CONTEXT_WINDOW_TOKENS,
                requested_output_tokens: u64::from(OPENAI_RESPONSES_PORTABLE_TARGET_OUTPUT_TOKENS),
                safety_buffer_tokens: OPENAI_RESPONSES_PORTABLE_TARGET_SAFETY_BUFFER_TOKENS,
            },
        };
        proof.validate_for(
            frozen_request.fingerprint(),
            TokenMeasurementScope::RenderedTargetInput,
            &binding,
        )?;
        Ok(PortableTargetRequestMaterial::new(
            frozen_request,
            binding,
            proof,
        ))
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let body = build_responses_request(&request)?;
        let url = self.responses_url();
        let response = timeout_provider_request(self.post_json(&url, &body), self.timeouts)
            .await
            .map_err(|phase| {
                provider_timeout_error(phase, self.timeouts, self.name(), &request.model_name)
            })?
            .context("OpenAI Responses request failed")?;
        let status = response.status();
        if status.is_success() {
            return Ok(response_stream(
                response,
                request.model_name.clone(),
                self.timeouts,
            ));
        }
        let status_code = status.as_u16();
        let error_body = read_error_response_body(
            response,
            self.timeouts.request_timeout,
            &SecretRedactor::from_values([self.api_key()?]),
            self.name(),
            &request.model_name,
            status_code,
        )
        .await?;
        Err(classify_status(status_code, error_body.text()).into())
    }
}

fn openai_responses_server_count_binding(model_name: &str) -> TokenMeasurementBinding {
    TokenMeasurementBinding {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        provider_name: "openai_responses".to_owned(),
        model_name: model_name.to_owned(),
        wire_profile: VersionedProfileIdentity::from_content(
            "openai-responses-input-token-count-wire",
            1,
            b"POST /v1/responses/input_tokens:TokenCountsBody(model,input,tools,tool_choice,reasoning);same prompt-bearing fields as POST /v1/responses",
        ),
        token_measurement_profile: VersionedProfileIdentity::from_content(
            "openai-responses-server-input-token-count",
            1,
            b"official /v1/responses/input_tokens returns response.input_tokens for the submitted request",
        ),
        hosted_parity_profile: Some(VersionedProfileIdentity::from_content(
            "openai-responses-server-count-parity",
            1,
            b"official pinned gpt-4.1-2025-04-14 server count over identical prompt-bearing Responses fields",
        )),
    }
}

impl OpenAiResponsesProvider {
    async fn post_json<T: serde::Serialize>(
        &self,
        url: &str,
        body: &T,
    ) -> Result<reqwest::Response> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let auth = format!("Bearer {}", self.api_key()?);
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth).context("invalid OpenAI Responses auth header")?,
        );
        if let Some(organization) = header_value(self.config.organization.as_deref())? {
            headers.insert("OpenAI-Organization", organization);
        }
        if let Some(project) = header_value(self.config.project.as_deref())? {
            headers.insert("OpenAI-Project", project);
        }
        self.client
            .post(url)
            .headers(headers)
            .json(body)
            .send()
            .await
            .context("failed to send OpenAI Responses request")
    }
}

async fn read_compaction_response_body(
    response: reqwest::Response,
    timeouts: ModelRequestTimeouts,
    model_name: &str,
) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut timeout_state = ProviderStreamTimeoutState::new(timeouts);
    let mut payload = Vec::new();
    loop {
        match timeout_provider_stream_next(&mut stream, timeouts, &mut timeout_state).await {
            Ok(Some(Ok(bytes))) => {
                let next_len = payload.len().saturating_add(bytes.len());
                if next_len > OPENAI_RESPONSES_COMPACT_RESPONSE_LIMIT_BYTES {
                    anyhow::bail!(
                        "OpenAI Responses compact response exceeds {OPENAI_RESPONSES_COMPACT_RESPONSE_LIMIT_BYTES} bytes"
                    );
                }
                payload.extend_from_slice(&bytes);
            }
            Ok(Some(Err(error))) => {
                return Err(error).context("failed to read OpenAI Responses compact response");
            }
            Err(phase) => {
                return Err(provider_timeout_error(
                    phase,
                    timeouts,
                    "openai_responses",
                    model_name,
                ));
            }
            Ok(None) => return Ok(payload),
        }
    }
}

async fn read_input_token_count_response_body(
    response: reqwest::Response,
    timeouts: ModelRequestTimeouts,
    model_name: &str,
) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut timeout_state = ProviderStreamTimeoutState::new(timeouts);
    let mut payload = Vec::new();
    loop {
        match timeout_provider_stream_next(&mut stream, timeouts, &mut timeout_state).await {
            Ok(Some(Ok(bytes))) => {
                let next_len = payload.len().saturating_add(bytes.len());
                if next_len > OPENAI_RESPONSES_INPUT_TOKEN_RESPONSE_LIMIT_BYTES {
                    anyhow::bail!(
                        "OpenAI Responses input-token response exceeds {OPENAI_RESPONSES_INPUT_TOKEN_RESPONSE_LIMIT_BYTES} bytes"
                    );
                }
                payload.extend_from_slice(&bytes);
            }
            Ok(Some(Err(error))) => {
                return Err(error).context("failed to read OpenAI Responses input-token response");
            }
            Err(phase) => {
                return Err(provider_timeout_error(
                    phase,
                    timeouts,
                    "openai_responses",
                    model_name,
                ));
            }
            Ok(None) => return Ok(payload),
        }
    }
}

fn parse_input_token_count(payload: &[u8]) -> Result<u64> {
    #[derive(serde::Deserialize)]
    struct InputTokenCountResponse {
        object: String,
        input_tokens: u64,
    }

    let response: InputTokenCountResponse = serde_json::from_slice(payload)
        .context("invalid OpenAI Responses input-token response JSON")?;
    if response.object != "response.input_tokens" {
        anyhow::bail!("OpenAI Responses input-token response has an unexpected object type");
    }
    Ok(response.input_tokens)
}

fn parse_compacted_window(payload: &[u8]) -> Result<OpenAiResponsesCompactedWindow> {
    #[derive(serde::Deserialize)]
    struct CompactResponse {
        id: String,
        object: String,
        output: Box<RawValue>,
    }

    let response: CompactResponse = serde_json::from_slice(payload)
        .context("invalid OpenAI Responses compact response JSON")?;
    if response.object != "response.compaction" {
        anyhow::bail!("OpenAI Responses compact response has an unexpected object type");
    }
    let response_id = response.id;
    if response_id.trim().is_empty() {
        anyhow::bail!("OpenAI Responses compact response is missing its id");
    }
    let output = response.output;
    let output_json = output.get().trim();
    if !output_json.starts_with('[') || !output_json.ends_with(']') || output_json == "[]" {
        anyhow::bail!("OpenAI Responses compact response is missing its canonical output window");
    }
    Ok(OpenAiResponsesCompactedWindow {
        response_id,
        output,
    })
}

fn header_value(value: Option<&str>) -> Result<Option<HeaderValue>> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(HeaderValue::from_str)
        .transpose()
        .context("invalid OpenAI Responses header value")
}

fn response_stream(
    response: reqwest::Response,
    model_name: String,
    timeouts: ModelRequestTimeouts,
) -> Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> {
    let state = (
        response.bytes_stream(),
        OpenAiResponsesSseDecoder::default(),
        StreamMapper::new(),
        VecDeque::<ProviderChunk>::new(),
        false,
        ProviderStreamTimeoutState::new(timeouts),
        timeouts,
        model_name,
    );
    Box::pin(stream::unfold(state, |mut state| async move {
        loop {
            if let Some(chunk) = state.3.pop_front() {
                return Some((Ok(chunk), state));
            }
            if state.4 {
                return None;
            }
            match timeout_provider_stream_next(&mut state.0, state.6, &mut state.5).await {
                Ok(Some(Ok(bytes))) => {
                    if let Err(error) =
                        enqueue_frames(&mut state.1, &mut state.2, &mut state.3, &bytes)
                    {
                        state.4 = true;
                        return Some((Err(error), state));
                    }
                    if state.2.is_completed() {
                        state.4 = true;
                    }
                }
                Ok(Some(Err(error))) => {
                    state.4 = true;
                    return Some((
                        Err(error).context("failed to read OpenAI Responses chunk"),
                        state,
                    ));
                }
                Err(phase) => {
                    state.4 = true;
                    return Some((
                        Err(provider_timeout_error(
                            phase,
                            state.6,
                            "openai_responses",
                            &state.7,
                        )),
                        state,
                    ));
                }
                Ok(None) => {
                    if let Err(error) =
                        enqueue_finished_frames(&mut state.1, &mut state.2, &mut state.3)
                    {
                        state.4 = true;
                        return Some((Err(error), state));
                    }
                    state.4 = true;
                    if !state.2.is_completed() {
                        return Some((
                            Err(anyhow::anyhow!(
                                "OpenAI Responses stream ended before response.completed"
                            )),
                            state,
                        ));
                    }
                }
            }
        }
    }))
}

fn provider_timeout_error(
    phase: ProviderTimeoutPhase,
    timeouts: ModelRequestTimeouts,
    provider: &str,
    model: &str,
) -> anyhow::Error {
    let metadata =
        ProviderTimeoutMetadata::new(phase, timeout_for_phase(phase, timeouts), provider, model);
    anyhow::anyhow!(
        "provider timeout: phase={} provider={} model={} timeout_ms={}",
        metadata.phase,
        metadata.provider,
        metadata.model,
        metadata.timeout_ms
    )
}
fn timeout_for_phase(phase: ProviderTimeoutPhase, timeouts: ModelRequestTimeouts) -> Duration {
    match phase {
        ProviderTimeoutPhase::RequestStart => timeouts.request_timeout,
        ProviderTimeoutPhase::StreamIdle => timeouts.stream_idle_timeout,
        ProviderTimeoutPhase::StreamTotal => timeouts
            .stream_total_timeout
            .unwrap_or(timeouts.stream_idle_timeout),
    }
}

fn enqueue_frames(
    decoder: &mut OpenAiResponsesSseDecoder,
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
    raw: impl AsRef<[u8]>,
) -> Result<()> {
    enqueue_decoded_frames(mapper, pending, decoder.push_bytes(raw.as_ref())?)
}

fn enqueue_finished_frames(
    decoder: &mut OpenAiResponsesSseDecoder,
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
) -> Result<()> {
    enqueue_decoded_frames(mapper, pending, decoder.finish()?)
}

fn enqueue_decoded_frames(
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
    frames: Vec<OpenAiResponsesSseFrame>,
) -> Result<()> {
    for frame in frames {
        if let OpenAiResponsesSseFrame::Event { event, data } = frame {
            let payload: Value =
                serde_json::from_str(&data).context("invalid OpenAI Responses stream JSON")?;
            pending.extend(mapper.map_event(&event, payload)?);
        }
    }
    Ok(())
}

async fn read_error_response_body(
    response: reqwest::Response,
    timeout: Duration,
    redactor: &SecretRedactor,
    provider: &str,
    model: &str,
    status: u16,
) -> Result<sigil_kernel::ProviderErrorBody> {
    read_provider_error_body(
        response.bytes_stream(),
        timeout,
        PROVIDER_ERROR_BODY_LIMIT_BYTES,
        redactor,
    )
    .await
    .with_context(|| {
        format!(
            "failed to read {provider} error response body for model {model} with status {status}"
        )
    })
}

#[cfg(test)]
#[path = "tests/provider_tests.rs"]
mod tests;

use std::{collections::VecDeque, pin::Pin, time::Duration};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, stream};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Serialize;
use tracing::debug;

use sigil_kernel::{
    CompletionRequest, ModelRequestTimeouts, PROVIDER_ERROR_BODY_LIMIT_BYTES, Provider,
    ProviderCapabilities, ProviderChunk, ProviderStreamTimeoutState, ProviderTimeoutMetadata,
    ProviderTimeoutPhase, SecretRedactor, read_provider_error_body, timeout_provider_request,
    timeout_provider_stream_next,
};

use crate::{
    capabilities::deepseek_capabilities,
    client::build_http_client,
    config::{DeepSeekProviderConfig, DeepSeekProviderProfile},
    endpoint::DeepSeekEndpointClass,
    errors::DeepSeekProviderError,
    fim::DeepSeekFimCompletionRequest,
    mapper::StreamMapper,
    models::{DeepSeekCompletionStreamEnvelope, DeepSeekStreamEnvelope},
    prefix::DeepSeekPrefixCompletionRequest,
    request::{
        build_chat_request, build_fim_completion_request, build_prefix_completion_request,
        extract_user_id, extract_user_id_from_partition_key,
    },
    retry::classify_status,
    stream::DeepSeekSseDecoder,
};

/// DeepSeek provider adapter that maps kernel requests onto DeepSeek transport flows.
#[derive(Clone)]
pub struct DeepSeekProvider {
    profile: DeepSeekProviderProfile,
    config: DeepSeekProviderConfig,
    timeouts: ModelRequestTimeouts,
    capabilities: ProviderCapabilities,
    client: reqwest::Client,
}

impl DeepSeekProvider {
    /// Builds a provider instance from parsed DeepSeek configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when the HTTP client or other transport prerequisites cannot be
    /// initialized from the provided config.
    pub fn new(config: DeepSeekProviderConfig, timeouts: ModelRequestTimeouts) -> Result<Self> {
        let config = config.resolved()?;
        let profile = config.profile();
        Ok(Self {
            profile,
            timeouts,
            client: build_http_client()?,
            capabilities: deepseek_capabilities(),
            config,
        })
    }

    fn api_key(&self) -> Result<String> {
        if let Some(api_key) = &self.config.api_key
            && !api_key.trim().is_empty()
        {
            return Ok(api_key.clone());
        }
        Err(DeepSeekProviderError::MissingApiKey.into())
    }

    fn base_url_for_endpoint(&self, endpoint: DeepSeekEndpointClass) -> &str {
        match endpoint {
            DeepSeekEndpointClass::Primary => &self.profile.primary_base_url,
            DeepSeekEndpointClass::Beta => &self.profile.beta_base_url,
            DeepSeekEndpointClass::AnthropicCompat => &self.profile.anthropic_base_url,
        }
    }

    /// Executes a DeepSeek prefix-completion flow and maps it into provider chunks.
    ///
    /// # Errors
    ///
    /// Returns an error when request shaping fails, user partition mapping fails, transport
    /// setup fails, or the DeepSeek backend returns an unrecoverable error.
    pub async fn stream_prefix_completion(
        &self,
        request: DeepSeekPrefixCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let user_id = extract_user_id_from_partition_key(
            request.traffic_partition_key.clone(),
            self.config.user_id_strategy.as_deref(),
        )?;
        let (endpoint, body) = build_prefix_completion_request(
            request,
            &self.profile.default_model,
            user_id,
            &self.profile.quirks,
        );
        self.stream_chat_chunks(endpoint, "/chat/completions", &body.model, &body)
            .await
    }

    /// Executes a DeepSeek fill-in-the-middle completion flow and maps it into provider chunks.
    ///
    /// # Errors
    ///
    /// Returns an error when request shaping fails, transport setup fails, or the DeepSeek
    /// backend returns an unrecoverable completion error.
    pub async fn stream_fim_completion(
        &self,
        request: DeepSeekFimCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let body = build_fim_completion_request(request, &self.profile.default_fim_model);
        self.stream_completion_chunks(
            DeepSeekEndpointClass::Beta,
            "/completions",
            &body.model,
            &body,
        )
        .await
    }
}

#[async_trait]
impl Provider for DeepSeekProvider {
    fn name(&self) -> &str {
        "deepseek"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.stream_chunks(request).await
    }
}

impl DeepSeekProvider {
    async fn stream_chunks(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let user_id = extract_user_id(&request, self.config.user_id_strategy.as_deref())?;
        let prepared = build_chat_request(
            &request,
            user_id,
            self.config.strict_tools_mode,
            &self.profile.quirks,
        )?;
        for diagnostic in &prepared.tool_diagnostics {
            debug!(
                target: "sigil_provider_deepseek",
                diagnostic_level = ?diagnostic.level,
                message = %diagnostic.message,
                "tool schema diagnostic"
            );
        }
        self.stream_chat_chunks(
            prepared.endpoint,
            "/chat/completions",
            &request.model_name,
            &prepared.body,
        )
        .await
    }

    async fn stream_chat_chunks<T: Serialize>(
        &self,
        endpoint: DeepSeekEndpointClass,
        path: &str,
        model_name: &str,
        body: &T,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let url = format!("{}{}", self.base_url_for_endpoint(endpoint), path);
        let response = timeout_provider_request(self.post_json(&url, body), self.timeouts)
            .await
            .map_err(|phase| provider_timeout_error(phase, self.timeouts, self.name(), model_name))?
            .context("deepseek request failed")?;
        let status = response.status();

        if status.is_success() {
            return Ok(chat_response_stream(
                response,
                model_name.to_owned(),
                self.timeouts,
            ));
        }
        let status_code = status.as_u16();
        let error_body = read_error_response_body(
            response,
            self.timeouts.request_timeout,
            &SecretRedactor::from_values([self.api_key()?]),
            self.name(),
            model_name,
            status_code,
        )
        .await?;
        Err(classify_status(status_code, error_body.text()).into())
    }

    async fn stream_completion_chunks<T: Serialize>(
        &self,
        endpoint: DeepSeekEndpointClass,
        path: &str,
        model_name: &str,
        body: &T,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let url = format!("{}{}", self.base_url_for_endpoint(endpoint), path);
        let response = timeout_provider_request(self.post_json(&url, body), self.timeouts)
            .await
            .map_err(|phase| provider_timeout_error(phase, self.timeouts, self.name(), model_name))?
            .context("deepseek completion request failed")?;
        let status = response.status();
        if !status.is_success() {
            let error_body = read_error_response_body(
                response,
                self.timeouts.request_timeout,
                &SecretRedactor::from_values([self.api_key()?]),
                self.name(),
                model_name,
                status.as_u16(),
            )
            .await?;
            return Err(classify_status(status.as_u16(), error_body.text()).into());
        }
        Ok(completion_response_stream(
            response,
            model_name.to_owned(),
            self.timeouts,
        ))
    }

    async fn post_json<T: Serialize>(&self, url: &str, body: &T) -> Result<reqwest::Response> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let auth = format!("Bearer {}", self.api_key()?);
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth).context("invalid auth header")?,
        );

        self.client
            .post(url)
            .headers(headers)
            .json(body)
            .send()
            .await
            .context("failed to send DeepSeek request")
    }
}

fn truncate_event_payload(payload: &str) -> String {
    const MAX_CHARS: usize = 240;
    let mut chars = payload.chars();
    let truncated = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
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

fn chat_response_stream(
    response: reqwest::Response,
    model_name: String,
    timeouts: ModelRequestTimeouts,
) -> Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> {
    let byte_stream = response.bytes_stream();
    let decoder = DeepSeekSseDecoder::default();
    let mapper = StreamMapper::new(model_name.clone());
    let pending = VecDeque::<ProviderChunk>::new();
    let finished = false;
    let saw_done = false;
    let timeout_state = ProviderStreamTimeoutState::new(timeouts);
    let state = (
        byte_stream,
        decoder,
        mapper,
        pending,
        finished,
        saw_done,
        timeout_state,
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

            match timeout_provider_stream_next(&mut state.0, state.7, &mut state.6).await {
                Ok(Some(Ok(bytes))) => {
                    match enqueue_chat_frames(&mut state.1, &mut state.2, &mut state.3, &bytes) {
                        Ok(done_seen) => {
                            state.5 |= done_seen;
                            if done_seen {
                                state.4 = true;
                            }
                        }
                        Err(error) => {
                            state.4 = true;
                            return Some((Err(error), state));
                        }
                    }
                }
                Ok(Some(Err(error))) => {
                    state.4 = true;
                    return Some((Err(error).context("failed to read response chunk"), state));
                }
                Err(phase) => {
                    state.4 = true;
                    return Some((
                        Err(provider_timeout_error(phase, state.7, "deepseek", &state.8)),
                        state,
                    ));
                }
                Ok(None) => {
                    match enqueue_finished_chat_frames(&mut state.1, &mut state.2, &mut state.3) {
                        Ok(done_seen) => {
                            state.5 |= done_seen;
                            if !state.5 {
                                state.3.push_back(ProviderChunk::Done);
                                state.5 = true;
                            }
                            state.4 = true;
                        }
                        Err(error) => {
                            state.4 = true;
                            return Some((Err(error), state));
                        }
                    }
                }
            }
        }
    }))
}

fn completion_response_stream(
    response: reqwest::Response,
    model_name: String,
    timeouts: ModelRequestTimeouts,
) -> Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> {
    let byte_stream = response.bytes_stream();
    let decoder = DeepSeekSseDecoder::default();
    let pending = VecDeque::<ProviderChunk>::new();
    let finished = false;
    let saw_done = false;
    let timeout_state = ProviderStreamTimeoutState::new(timeouts);
    let state = (
        byte_stream,
        decoder,
        pending,
        finished,
        saw_done,
        model_name,
        timeout_state,
        timeouts,
    );

    Box::pin(stream::unfold(state, |mut state| async move {
        loop {
            if let Some(chunk) = state.2.pop_front() {
                return Some((Ok(chunk), state));
            }
            if state.3 {
                return None;
            }

            match timeout_provider_stream_next(&mut state.0, state.7, &mut state.6).await {
                Ok(Some(Ok(bytes))) => {
                    match enqueue_completion_frames(&mut state.1, &mut state.2, &bytes, &state.5) {
                        Ok(done_seen) => {
                            state.4 |= done_seen;
                            if done_seen {
                                state.3 = true;
                            }
                        }
                        Err(error) => {
                            state.3 = true;
                            return Some((Err(error), state));
                        }
                    }
                }
                Ok(Some(Err(error))) => {
                    state.3 = true;
                    return Some((Err(error).context("failed to read completion chunk"), state));
                }
                Err(phase) => {
                    state.3 = true;
                    return Some((
                        Err(provider_timeout_error(phase, state.7, "deepseek", &state.5)),
                        state,
                    ));
                }
                Ok(None) => {
                    match enqueue_finished_completion_frames(&mut state.1, &mut state.2, &state.5) {
                        Ok(done_seen) => {
                            state.4 |= done_seen;
                            if !state.4 {
                                state.2.push_back(ProviderChunk::Done);
                                state.4 = true;
                            }
                            state.3 = true;
                        }
                        Err(error) => {
                            state.3 = true;
                            return Some((Err(error), state));
                        }
                    }
                }
            }
        }
    }))
}

fn enqueue_chat_frames(
    decoder: &mut DeepSeekSseDecoder,
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
    raw: impl AsRef<[u8]>,
) -> Result<bool> {
    let mut done_seen = false;
    for frame in decoder.push_bytes(raw.as_ref())? {
        done_seen |= enqueue_chat_frame(mapper, pending, frame)?;
    }
    Ok(done_seen)
}

fn enqueue_finished_chat_frames(
    decoder: &mut DeepSeekSseDecoder,
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
) -> Result<bool> {
    let mut done_seen = false;
    for frame in decoder.finish()? {
        done_seen |= enqueue_chat_frame(mapper, pending, frame)?;
    }
    Ok(done_seen)
}

fn enqueue_chat_frame(
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
    frame: crate::response::DeepSeekSseFrame,
) -> Result<bool> {
    match frame {
        crate::response::DeepSeekSseFrame::Blank | crate::response::DeepSeekSseFrame::Comment => {
            Ok(false)
        }
        crate::response::DeepSeekSseFrame::Done => {
            pending.push_back(ProviderChunk::Done);
            Ok(true)
        }
        crate::response::DeepSeekSseFrame::Data(data) => {
            let envelope: DeepSeekStreamEnvelope =
                serde_json::from_str(&data).with_context(|| {
                    format!("invalid DeepSeek event {}", truncate_event_payload(&data))
                })?;
            pending.extend(mapper.map_envelope(envelope)?);
            Ok(false)
        }
    }
}

fn enqueue_completion_frames(
    decoder: &mut DeepSeekSseDecoder,
    pending: &mut VecDeque<ProviderChunk>,
    raw: impl AsRef<[u8]>,
    model_name: &str,
) -> Result<bool> {
    let mut done_seen = false;
    for frame in decoder.push_bytes(raw.as_ref())? {
        done_seen |= enqueue_completion_frame(pending, frame, model_name)?;
        if done_seen {
            break;
        }
    }
    Ok(done_seen)
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

fn enqueue_finished_completion_frames(
    decoder: &mut DeepSeekSseDecoder,
    pending: &mut VecDeque<ProviderChunk>,
    model_name: &str,
) -> Result<bool> {
    let mut done_seen = false;
    for frame in decoder.finish()? {
        done_seen |= enqueue_completion_frame(pending, frame, model_name)?;
        if done_seen {
            break;
        }
    }
    Ok(done_seen)
}

fn enqueue_completion_frame(
    pending: &mut VecDeque<ProviderChunk>,
    frame: crate::response::DeepSeekSseFrame,
    model_name: &str,
) -> Result<bool> {
    match frame {
        crate::response::DeepSeekSseFrame::Blank | crate::response::DeepSeekSseFrame::Comment => {
            Ok(false)
        }
        crate::response::DeepSeekSseFrame::Done => {
            pending.push_back(ProviderChunk::Done);
            Ok(true)
        }
        crate::response::DeepSeekSseFrame::Data(data) => {
            let envelope: DeepSeekCompletionStreamEnvelope = serde_json::from_str(&data)
                .with_context(|| {
                    format!(
                        "invalid DeepSeek completion event {}",
                        truncate_event_payload(&data)
                    )
                })?;
            let mut done_seen = false;
            let mut frame_done = false;
            for choice in envelope.choices {
                if let Some(text) = choice.text {
                    pending.push_back(ProviderChunk::TextDelta(text));
                }
                if matches!(choice.finish_reason.as_deref(), Some("stop")) {
                    frame_done = true;
                }
            }
            if let Some(usage) = envelope.usage {
                pending.push_back(ProviderChunk::Usage(crate::pricing::enrich_usage_costs(
                    model_name,
                    sigil_kernel::UsageStats {
                        prompt_tokens: usage.prompt_tokens,
                        completion_tokens: usage.completion_tokens,
                        cache_hit_tokens: usage.prompt_cache_hit_tokens.unwrap_or_default(),
                        cache_miss_tokens: usage.prompt_cache_miss_tokens.unwrap_or_default(),
                        input_cost: 0.0,
                        output_cost: 0.0,
                        cache_savings: 0.0,
                        system_fingerprint: envelope.system_fingerprint.clone(),
                    },
                )));
            }
            if frame_done {
                pending.push_back(ProviderChunk::Done);
                done_seen = true;
            }
            Ok(done_seen)
        }
    }
}

#[cfg(test)]
#[path = "tests/provider_tests.rs"]
mod tests;

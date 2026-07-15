use std::{collections::VecDeque, pin::Pin, time::Duration};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, stream};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};

use sigil_kernel::{
    CompletionRequest, HostedWebSearchCapability, ImageInputCapability, ModelRequestTimeouts,
    PROVIDER_ERROR_BODY_LIMIT_BYTES, Provider, ProviderCapabilities, ProviderChunk,
    ProviderStreamTimeoutState, ProviderTimeoutMetadata, ProviderTimeoutPhase, SecretRedactor,
    read_provider_error_body, timeout_provider_request, timeout_provider_stream_next,
};

use crate::{
    capabilities::anthropic_capabilities,
    client::build_http_client,
    config::AnthropicProviderConfig,
    errors::{AnthropicProviderError, classify_status},
    hosted_search::{
        AnthropicHostedContinuationStore, AnthropicHostedPlatform, AnthropicHostedStreamContext,
        hosted_web_search_capability, hosted_web_search_request, is_hosted_web_search_model,
    },
    mapper::StreamMapper,
    models::AnthropicStreamEnvelope,
    request::{anthropic_image_input_capability, build_messages_request_with_continuations},
    stream::{AnthropicSseDecoder, AnthropicSseFrame},
};

#[derive(Clone)]
pub struct AnthropicProvider {
    pub(crate) config: AnthropicProviderConfig,
    pub(crate) timeouts: ModelRequestTimeouts,
    capabilities: ProviderCapabilities,
    client: reqwest::Client,
    hosted_continuations: AnthropicHostedContinuationStore,
    pub(crate) hosted_platform: AnthropicHostedPlatform,
}

impl AnthropicProvider {
    /// Builds a provider instance from parsed Anthropic configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when environment overrides are invalid or the HTTP client cannot be built.
    pub fn new(config: AnthropicProviderConfig, timeouts: ModelRequestTimeouts) -> Result<Self> {
        let config = config.resolved()?;
        let hosted_platform = AnthropicHostedPlatform::from_base_url(&config.base_url);
        Ok(Self {
            timeouts,
            client: build_http_client()?,
            capabilities: anthropic_capabilities(),
            config,
            hosted_continuations: AnthropicHostedContinuationStore::default(),
            hosted_platform,
        })
    }

    pub(crate) fn api_key(&self) -> Result<String> {
        if let Some(api_key) = &self.config.api_key
            && !api_key.trim().is_empty()
        {
            return Ok(api_key.clone());
        }
        Err(AnthropicProviderError::MissingApiKey.into())
    }

    pub(crate) fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'))
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    fn image_input_capability(&self, model_name: &str) -> ImageInputCapability {
        anthropic_image_input_capability(model_name)
    }

    fn hosted_web_search_capability(&self, model_name: &str) -> HostedWebSearchCapability {
        hosted_web_search_capability(model_name, self.hosted_platform)
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let hosted_search = hosted_web_search_request(&request.hosted_tools)?;
        if hosted_search.is_some() {
            if !self.hosted_platform.supports_web_search() {
                return Err(AnthropicProviderError::UnsupportedHostedWebSearchPlatform.into());
            }
            if !is_hosted_web_search_model(&request.model_name) {
                return Err(AnthropicProviderError::UnsupportedHostedWebSearchModel(
                    request.model_name.clone(),
                )
                .into());
            }
        }
        let prepared = build_messages_request_with_continuations(
            &request,
            self.config.max_tokens,
            &self.hosted_continuations,
        )?;
        let hosted_context = hosted_search.map(|hosted_search| AnthropicHostedStreamContext {
            authorization_id: hosted_search.authorization_id.clone(),
            continuation_store: self.hosted_continuations.clone(),
            prior_invocations: prepared.prior_hosted_invocations.clone(),
        });
        let body = prepared.body;
        let url = self.messages_url();
        let response = timeout_provider_request(self.post_json(&url, &body), self.timeouts)
            .await
            .map_err(|phase| {
                provider_timeout_error(phase, self.timeouts, self.name(), &request.model_name)
            })?
            .context("Anthropic request failed")?;
        let status = response.status();
        if status.is_success() {
            return Ok(response_stream(
                response,
                request.model_name.clone(),
                self.timeouts,
                hosted_context,
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

impl AnthropicProvider {
    async fn post_json<T: serde::Serialize>(
        &self,
        url: &str,
        body: &T,
    ) -> Result<reqwest::Response> {
        self.post_json_with_required_beta(url, body, &[]).await
    }

    pub(crate) async fn post_json_with_required_beta<T: serde::Serialize>(
        &self,
        url: &str,
        body: &T,
        required_beta_headers: &[&str],
    ) -> Result<reqwest::Response> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&self.api_key()?).context("invalid Anthropic API key header")?,
        );
        headers.insert(
            "anthropic-version",
            HeaderValue::from_str(self.config.anthropic_version.trim())
                .context("invalid Anthropic version header")?,
        );
        let mut beta_headers = self.config.beta_headers.clone();
        for required in required_beta_headers {
            if !beta_headers
                .iter()
                .any(|configured| configured.trim() == *required)
            {
                beta_headers.push((*required).to_owned());
            }
        }
        if let Some(beta) = beta_header(&beta_headers)? {
            headers.insert("anthropic-beta", beta);
        }

        self.client
            .post(url)
            .headers(headers)
            .json(body)
            .send()
            .await
            .context("failed to send Anthropic request")
    }
}

fn beta_header(beta_headers: &[String]) -> Result<Option<HeaderValue>> {
    let value = beta_headers
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    if value.is_empty() {
        Ok(None)
    } else {
        HeaderValue::from_str(&value)
            .map(Some)
            .context("invalid Anthropic beta header")
    }
}

fn response_stream(
    response: reqwest::Response,
    model_name: String,
    timeouts: ModelRequestTimeouts,
    hosted_context: Option<AnthropicHostedStreamContext>,
) -> Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> {
    let byte_stream = response.bytes_stream();
    let decoder = AnthropicSseDecoder::default();
    let mapper = StreamMapper::new(hosted_context);
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
                    match enqueue_frames(&mut state.1, &mut state.2, &mut state.3, &bytes) {
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
                        Err(provider_timeout_error(
                            phase,
                            state.7,
                            "anthropic",
                            &state.8,
                        )),
                        state,
                    ));
                }
                Ok(None) => match enqueue_finished_frames(&mut state.1, &mut state.2, &mut state.3)
                {
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
                },
            }
        }
    }))
}

pub(crate) fn provider_timeout_error(
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
    decoder: &mut AnthropicSseDecoder,
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
    raw: impl AsRef<[u8]>,
) -> Result<bool> {
    let frames = decoder.push_bytes(raw.as_ref())?;
    enqueue_decoded_frames(mapper, pending, frames)
}

pub(crate) async fn read_error_response_body(
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

fn enqueue_finished_frames(
    decoder: &mut AnthropicSseDecoder,
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
) -> Result<bool> {
    let frames = decoder.finish()?;
    let done_seen = enqueue_decoded_frames(mapper, pending, frames)?;
    if !done_seen {
        pending.extend(mapper.finish()?);
    }
    Ok(done_seen)
}

fn enqueue_decoded_frames(
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
    frames: Vec<AnthropicSseFrame>,
) -> Result<bool> {
    let mut done_seen = false;
    for frame in frames {
        match frame {
            AnthropicSseFrame::Data(data) => {
                let envelope: AnthropicStreamEnvelope =
                    serde_json::from_str(&data).context("invalid Anthropic stream JSON")?;
                let chunks = mapper.map_envelope(envelope)?;
                done_seen |= chunks
                    .iter()
                    .any(|chunk| matches!(chunk, ProviderChunk::Done));
                pending.extend(chunks);
            }
            AnthropicSseFrame::Comment | AnthropicSseFrame::Blank => {}
        }
    }
    Ok(done_seen)
}

#[cfg(test)]
#[path = "tests/provider_tests.rs"]
mod tests;

use std::{collections::VecDeque, pin::Pin, time::Duration};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, stream};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};

use sigil_kernel::{
    CompletionRequest, ModelRequestTimeouts, PROVIDER_ERROR_BODY_LIMIT_BYTES, Provider,
    ProviderCapabilities, ProviderChunk, ProviderStreamTimeoutState, ProviderTimeoutMetadata,
    ProviderTimeoutPhase, SecretRedactor, read_provider_error_body, timeout_provider_request,
    timeout_provider_stream_next,
};

use crate::{
    capabilities::openai_compatible_capabilities,
    client::build_http_client,
    config::OpenAiCompatibleProviderConfig,
    errors::{OpenAiCompatibleProviderError, classify_status},
    mapper::StreamMapper,
    models::OpenAiStreamEnvelope,
    request::build_chat_request,
    stream::{OpenAiSseDecoder, OpenAiSseFrame},
};

#[derive(Clone)]
pub struct OpenAiCompatibleProvider {
    config: OpenAiCompatibleProviderConfig,
    timeouts: ModelRequestTimeouts,
    capabilities: ProviderCapabilities,
    client: reqwest::Client,
}

impl OpenAiCompatibleProvider {
    /// Builds a provider instance from parsed OpenAI-compatible configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when environment overrides are invalid or the HTTP client cannot be built.
    pub fn new(
        config: OpenAiCompatibleProviderConfig,
        timeouts: ModelRequestTimeouts,
    ) -> Result<Self> {
        let config = config.resolved()?;
        Ok(Self {
            timeouts,
            client: build_http_client()?,
            capabilities: openai_compatible_capabilities(),
            config,
        })
    }

    fn api_key(&self) -> Result<String> {
        if let Some(api_key) = &self.config.api_key
            && !api_key.trim().is_empty()
        {
            return Ok(api_key.clone());
        }
        Err(OpenAiCompatibleProviderError::MissingApiKey.into())
    }

    fn chat_completions_url(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        )
    }
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        "openai_compat"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let body = build_chat_request(&request)?;
        let url = self.chat_completions_url();
        let response = timeout_provider_request(self.post_json(&url, &body), self.timeouts)
            .await
            .map_err(|phase| {
                provider_timeout_error(phase, self.timeouts, self.name(), &request.model_name)
            })?
            .context("OpenAI-compatible request failed")?;
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

impl OpenAiCompatibleProvider {
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
            HeaderValue::from_str(&auth).context("invalid auth header")?,
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
            .context("failed to send OpenAI-compatible request")
    }
}

fn header_value(value: Option<&str>) -> Result<Option<HeaderValue>> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(HeaderValue::from_str)
        .transpose()
        .context("invalid OpenAI-compatible header value")
}

fn response_stream(
    response: reqwest::Response,
    model_name: String,
    timeouts: ModelRequestTimeouts,
) -> Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> {
    let byte_stream = response.bytes_stream();
    let decoder = OpenAiSseDecoder::default();
    let mapper = StreamMapper::new();
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
                            "openai_compat",
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
    decoder: &mut OpenAiSseDecoder,
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
    raw: impl AsRef<[u8]>,
) -> Result<bool> {
    let frames = decoder.push_bytes(raw.as_ref())?;
    enqueue_decoded_frames(mapper, pending, frames)
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

fn enqueue_finished_frames(
    decoder: &mut OpenAiSseDecoder,
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
) -> Result<bool> {
    let frames = decoder.finish()?;
    enqueue_decoded_frames(mapper, pending, frames)
}

fn enqueue_decoded_frames(
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
    frames: Vec<OpenAiSseFrame>,
) -> Result<bool> {
    let mut done_seen = false;
    for frame in frames {
        match frame {
            OpenAiSseFrame::Data(data) => {
                let envelope: OpenAiStreamEnvelope =
                    serde_json::from_str(&data).context("invalid OpenAI-compatible stream JSON")?;
                pending.extend(mapper.map_envelope(envelope)?);
            }
            OpenAiSseFrame::Done => {
                pending.extend(mapper.finish());
                pending.push_back(ProviderChunk::Done);
                done_seen = true;
            }
            OpenAiSseFrame::Comment | OpenAiSseFrame::Blank => {}
        }
    }
    Ok(done_seen)
}

#[cfg(test)]
#[path = "tests/provider_tests.rs"]
mod tests;

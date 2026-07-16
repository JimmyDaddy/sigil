use std::{collections::VecDeque, pin::Pin, time::Duration};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, stream};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};

use sigil_kernel::{
    CompletionRequest, HostedCitationFidelity, HostedConstraintEnforcement,
    HostedCustomToolCompatibility, HostedQueryVisibility, HostedRequestWireState,
    HostedSourceFidelity, HostedToolSupport, HostedWebSearchCapability, ImageInputCapability,
    ModelRequestTimeouts, PROVIDER_ERROR_BODY_LIMIT_BYTES, Provider, ProviderCapabilities,
    ProviderChunk, ProviderStreamTimeoutState, ProviderTimeoutMetadata, ProviderTimeoutPhase,
    SecretRedactor, read_provider_error_body, timeout_provider_request,
    timeout_provider_stream_next,
};

use crate::{
    capabilities::gemini_capabilities,
    client::build_http_client,
    config::GeminiProviderConfig,
    errors::{GeminiProviderError, classify_status},
    hosted_search::{
        gemini_hosted_custom_tools_supported, gemini_hosted_web_search_supported, hosted_invocation,
    },
    mapper::StreamMapper,
    models::GeminiStreamEnvelope,
    request::{build_generate_content_request, gemini_image_input_capability},
    stream::{GeminiSseDecoder, GeminiSseFrame},
};

#[derive(Clone)]
pub struct GeminiProvider {
    config: GeminiProviderConfig,
    timeouts: ModelRequestTimeouts,
    capabilities: ProviderCapabilities,
    client: reqwest::Client,
}

impl GeminiProvider {
    /// Builds a provider instance from parsed Gemini configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when environment overrides are invalid or the HTTP client cannot be built.
    pub fn new(config: GeminiProviderConfig, timeouts: ModelRequestTimeouts) -> Result<Self> {
        let config = config.resolved()?;
        Ok(Self {
            timeouts,
            client: build_http_client()?,
            capabilities: gemini_capabilities(),
            config,
        })
    }

    fn api_key(&self) -> Result<String> {
        if let Some(api_key) = &self.config.api_key
            && !api_key.trim().is_empty()
        {
            return Ok(api_key.clone());
        }
        Err(GeminiProviderError::MissingApiKey.into())
    }

    fn stream_generate_content_url(&self, request_model: &str) -> String {
        let model = request_model.trim();
        let model = if model.is_empty() {
            self.config.model.as_str()
        } else {
            model
        };
        let model_path = if model.starts_with("models/") {
            model.to_owned()
        } else {
            format!("models/{model}")
        };
        format!(
            "{}/{}:streamGenerateContent",
            self.config.base_url.trim_end_matches('/'),
            model_path
        )
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    fn name(&self) -> &str {
        "gemini"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    fn image_input_capability(&self, model_name: &str) -> ImageInputCapability {
        gemini_image_input_capability(model_name)
    }

    fn hosted_web_search_capability(&self, model_name: &str) -> HostedWebSearchCapability {
        if gemini_hosted_web_search_supported(model_name) {
            HostedWebSearchCapability {
                support: HostedToolSupport::ServerManaged,
                query_visibility: HostedQueryVisibility::ProviderReportedPostExecution,
                source_fidelity: HostedSourceFidelity::UrlAndTitle,
                citation_fidelity: HostedCitationFidelity::OutputSpan,
                max_uses_enforcement: HostedConstraintEnforcement::Unsupported,
                domain_filter_enforcement: HostedConstraintEnforcement::Unsupported,
                custom_tool_compatibility: if gemini_hosted_custom_tools_supported(model_name) {
                    HostedCustomToolCompatibility::Supported
                } else {
                    HostedCustomToolCompatibility::Unsupported
                },
            }
        } else {
            HostedWebSearchCapability::default()
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let hosted_invocation = hosted_invocation(&request.hosted_tools)?;
        let hosted_enabled = hosted_invocation.is_some();
        if hosted_enabled
            && !self
                .hosted_web_search_capability(&request.model_name)
                .is_supported()
        {
            anyhow::bail!("Gemini model does not support hosted web search");
        }
        let body = build_generate_content_request(&request)?;
        let url = self.stream_generate_content_url(&request.model_name);
        let mut hosted_wire_state = HostedRequestWireState::Prepared;
        let response = timeout_provider_request(
            self.post_json(
                &url,
                &body,
                hosted_enabled.then_some(&mut hosted_wire_state),
            ),
            self.timeouts,
        )
        .await
        .map_err(|phase| {
            provider_timeout_error(phase, self.timeouts, self.name(), &request.model_name)
        })?
        .context("Gemini request failed")?;
        let status = response.status();
        if status.is_success() {
            return Ok(response_stream(
                response,
                request.model_name.clone(),
                self.timeouts,
                hosted_invocation,
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

impl GeminiProvider {
    async fn post_json<T: serde::Serialize>(
        &self,
        url: &str,
        body: &T,
        hosted_wire_state: Option<&mut HostedRequestWireState>,
    ) -> Result<reqwest::Response> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "x-goog-api-key",
            HeaderValue::from_str(&self.api_key()?).context("invalid Gemini API key header")?,
        );

        let request = self
            .client
            .post(url)
            .query(&[("alt", "sse")])
            .headers(headers)
            .json(body);
        match request.send().await {
            Ok(response) => {
                if let Some(wire_state) = hosted_wire_state {
                    wire_state
                        .mark_request_bytes_started()
                        .context("invalid Gemini hosted request wire state")?;
                }
                Ok(response)
            }
            Err(error) => {
                if !error.is_connect()
                    && let Some(wire_state) = hosted_wire_state
                {
                    wire_state
                        .mark_request_bytes_started()
                        .context("invalid Gemini hosted request wire state")?;
                }
                Err(error).context("failed to send Gemini request")
            }
        }
    }
}

fn response_stream(
    response: reqwest::Response,
    model_name: String,
    timeouts: ModelRequestTimeouts,
    hosted_invocation: Option<crate::hosted_search::GeminiHostedInvocation>,
) -> Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> {
    let byte_stream = response.bytes_stream();
    let decoder = GeminiSseDecoder::default();
    let mapper = hosted_invocation.map_or_else(StreamMapper::new, StreamMapper::with_hosted);
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
                        Err(provider_timeout_error(phase, state.7, "gemini", &state.8)),
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
    decoder: &mut GeminiSseDecoder,
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
    decoder: &mut GeminiSseDecoder,
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
) -> Result<bool> {
    let frames = decoder.finish()?;
    let done_seen = enqueue_decoded_frames(mapper, pending, frames)?;
    pending.extend(mapper.finish());
    Ok(done_seen)
}

fn enqueue_decoded_frames(
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
    frames: Vec<GeminiSseFrame>,
) -> Result<bool> {
    let mut done_seen = false;
    for frame in frames {
        match frame {
            GeminiSseFrame::Data(data) => {
                let envelope: GeminiStreamEnvelope =
                    serde_json::from_str(&data).context("invalid Gemini stream JSON")?;
                pending.extend(mapper.map_envelope(envelope)?);
            }
            GeminiSseFrame::Done => {
                pending.extend(mapper.finish());
                pending.push_back(ProviderChunk::Done);
                done_seen = true;
            }
            GeminiSseFrame::Comment | GeminiSseFrame::Blank => {}
        }
    }
    Ok(done_seen)
}

#[cfg(test)]
#[path = "tests/provider_tests.rs"]
mod tests;

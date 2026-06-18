use std::{collections::VecDeque, pin::Pin};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, StreamExt, stream};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};

use sigil_kernel::{CompletionRequest, Provider, ProviderCapabilities, ProviderChunk};

use crate::{
    capabilities::gemini_capabilities,
    client::build_http_client,
    config::GeminiProviderConfig,
    errors::{GeminiProviderError, classify_status},
    mapper::StreamMapper,
    models::GeminiStreamEnvelope,
    request::build_generate_content_request,
    stream::{GeminiSseDecoder, GeminiSseFrame},
};

const GEMINI_MAX_ATTEMPTS: usize = 2;

#[derive(Clone)]
pub struct GeminiProvider {
    config: GeminiProviderConfig,
    capabilities: ProviderCapabilities,
    client: reqwest::Client,
}

impl GeminiProvider {
    /// Builds a provider instance from parsed Gemini configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when environment overrides are invalid or the HTTP client cannot be built.
    pub fn new(config: GeminiProviderConfig) -> Result<Self> {
        let config = config.resolved()?;
        Ok(Self {
            client: build_http_client(config.request_timeout_secs)?,
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

    fn stream_generate_content_url(&self) -> String {
        let model_path = if self.config.model.starts_with("models/") {
            self.config.model.clone()
        } else {
            format!("models/{}", self.config.model)
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

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let body = build_generate_content_request(&request)?;
        let url = self.stream_generate_content_url();
        let mut attempt = 0usize;
        loop {
            attempt += 1;
            let response = self
                .post_json(&url, &body)
                .await
                .context("Gemini request failed")?;
            let status = response.status();
            if status.is_success() {
                return Ok(response_stream(response));
            }
            let error_body = response.text().await.unwrap_or_default();
            let error = classify_status(status.as_u16(), &error_body);
            if attempt < GEMINI_MAX_ATTEMPTS && is_retryable_status(&error) {
                continue;
            }
            return Err(error.into());
        }
    }
}

fn is_retryable_status(error: &GeminiProviderError) -> bool {
    matches!(
        error,
        GeminiProviderError::RateLimited | GeminiProviderError::RetryableStatus(_)
    )
}

impl GeminiProvider {
    async fn post_json<T: serde::Serialize>(
        &self,
        url: &str,
        body: &T,
    ) -> Result<reqwest::Response> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        self.client
            .post(url)
            .query(&[("alt", "sse"), ("key", self.api_key()?.as_str())])
            .headers(headers)
            .json(body)
            .send()
            .await
            .context("failed to send Gemini request")
    }
}

fn response_stream(
    response: reqwest::Response,
) -> Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> {
    let byte_stream = response.bytes_stream();
    let decoder = GeminiSseDecoder::default();
    let mapper = StreamMapper::new();
    let pending = VecDeque::<ProviderChunk>::new();
    let finished = false;
    let saw_done = false;
    let state = (byte_stream, decoder, mapper, pending, finished, saw_done);

    Box::pin(stream::unfold(state, |mut state| async move {
        loop {
            if let Some(chunk) = state.3.pop_front() {
                return Some((Ok(chunk), state));
            }
            if state.4 {
                return None;
            }

            match state.0.next().await {
                Some(Ok(bytes)) => {
                    let raw = match String::from_utf8(bytes.to_vec()) {
                        Ok(raw) => raw,
                        Err(error) => {
                            state.4 = true;
                            return Some((Err(error).context("invalid UTF-8 SSE chunk"), state));
                        }
                    };
                    match enqueue_frames(&mut state.1, &mut state.2, &mut state.3, &raw) {
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
                Some(Err(error)) => {
                    state.4 = true;
                    return Some((Err(error).context("failed to read response chunk"), state));
                }
                None => match enqueue_finished_frames(&mut state.1, &mut state.2, &mut state.3) {
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

fn enqueue_frames(
    decoder: &mut GeminiSseDecoder,
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
    raw: &str,
) -> Result<bool> {
    let frames = decoder.push(raw)?;
    enqueue_decoded_frames(mapper, pending, frames)
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

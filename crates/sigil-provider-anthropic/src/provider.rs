use std::{collections::VecDeque, pin::Pin};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, StreamExt, stream};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};

use sigil_kernel::{CompletionRequest, Provider, ProviderCapabilities, ProviderChunk};

use crate::{
    capabilities::anthropic_capabilities,
    client::build_http_client,
    config::AnthropicProviderConfig,
    errors::{AnthropicProviderError, classify_status},
    mapper::StreamMapper,
    models::AnthropicStreamEnvelope,
    request::build_messages_request,
    stream::{AnthropicSseDecoder, AnthropicSseFrame},
};

const ANTHROPIC_MAX_ATTEMPTS: usize = 2;

#[derive(Clone)]
pub struct AnthropicProvider {
    config: AnthropicProviderConfig,
    capabilities: ProviderCapabilities,
    client: reqwest::Client,
}

impl AnthropicProvider {
    /// Builds a provider instance from parsed Anthropic configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when environment overrides are invalid or the HTTP client cannot be built.
    pub fn new(config: AnthropicProviderConfig) -> Result<Self> {
        let config = config.resolved()?;
        Ok(Self {
            client: build_http_client(config.request_timeout_secs)?,
            capabilities: anthropic_capabilities(),
            config,
        })
    }

    fn api_key(&self) -> Result<String> {
        if let Some(api_key) = &self.config.api_key
            && !api_key.trim().is_empty()
        {
            return Ok(api_key.clone());
        }
        Err(AnthropicProviderError::MissingApiKey.into())
    }

    fn messages_url(&self) -> String {
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

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let body = build_messages_request(&request, self.config.max_tokens)?;
        let url = self.messages_url();
        let mut attempt = 0usize;
        loop {
            attempt += 1;
            let response = self
                .post_json(&url, &body)
                .await
                .context("Anthropic request failed")?;
            let status = response.status();
            if status.is_success() {
                return Ok(response_stream(response));
            }
            let error_body = response.text().await.unwrap_or_default();
            let error = classify_status(status.as_u16(), &error_body);
            if attempt < ANTHROPIC_MAX_ATTEMPTS && is_retryable_status(&error) {
                continue;
            }
            return Err(error.into());
        }
    }
}

fn is_retryable_status(error: &AnthropicProviderError) -> bool {
    matches!(
        error,
        AnthropicProviderError::RateLimited | AnthropicProviderError::RetryableStatus(_)
    )
}

impl AnthropicProvider {
    async fn post_json<T: serde::Serialize>(
        &self,
        url: &str,
        body: &T,
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
        if let Some(beta) = beta_header(&self.config.beta_headers)? {
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
) -> Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> {
    let byte_stream = response.bytes_stream();
    let decoder = AnthropicSseDecoder::default();
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
    decoder: &mut AnthropicSseDecoder,
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
    raw: &str,
) -> Result<bool> {
    let frames = decoder.push(raw)?;
    enqueue_decoded_frames(mapper, pending, frames)
}

fn enqueue_finished_frames(
    decoder: &mut AnthropicSseDecoder,
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
) -> Result<bool> {
    let frames = decoder.finish()?;
    let done_seen = enqueue_decoded_frames(mapper, pending, frames)?;
    if !done_seen {
        pending.extend(mapper.finish());
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

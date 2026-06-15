use std::{collections::VecDeque, pin::Pin};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, StreamExt, stream};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};

use sigil_kernel::{CompletionRequest, Provider, ProviderCapabilities, ProviderChunk};

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

const OPENAI_COMPAT_MAX_ATTEMPTS: usize = 2;

#[derive(Clone)]
pub struct OpenAiCompatibleProvider {
    config: OpenAiCompatibleProviderConfig,
    capabilities: ProviderCapabilities,
    client: reqwest::Client,
}

impl OpenAiCompatibleProvider {
    /// Builds a provider instance from parsed OpenAI-compatible configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when environment overrides are invalid or the HTTP client cannot be built.
    pub fn new(config: OpenAiCompatibleProviderConfig) -> Result<Self> {
        let config = config.resolved()?;
        Ok(Self {
            client: build_http_client(config.request_timeout_secs)?,
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
        let mut attempt = 0usize;
        loop {
            attempt += 1;
            let response = self
                .post_json(&url, &body)
                .await
                .context("OpenAI-compatible request failed")?;
            let status = response.status();
            if status.is_success() {
                return Ok(response_stream(response));
            }
            let error_body = response.text().await.unwrap_or_default();
            let error = classify_status(status.as_u16(), &error_body);
            if attempt < OPENAI_COMPAT_MAX_ATTEMPTS && is_retryable_status(&error) {
                continue;
            }
            return Err(error.into());
        }
    }
}

fn is_retryable_status(error: &OpenAiCompatibleProviderError) -> bool {
    matches!(
        error,
        OpenAiCompatibleProviderError::RateLimited
            | OpenAiCompatibleProviderError::RetryableStatus(_)
    )
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
) -> Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> {
    let byte_stream = response.bytes_stream();
    let decoder = OpenAiSseDecoder::default();
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
    decoder: &mut OpenAiSseDecoder,
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
    raw: &str,
) -> Result<bool> {
    let frames = decoder.push(raw)?;
    enqueue_decoded_frames(mapper, pending, frames)
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

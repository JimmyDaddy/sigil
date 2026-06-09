use std::{collections::VecDeque, pin::Pin};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, StreamExt, stream};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Serialize;
use tokio::time::{Duration, sleep};
use tracing::{debug, warn};

use termquill_kernel::{CompletionRequest, Provider, ProviderCapabilities, ProviderChunk};

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
    pub fn new(config: DeepSeekProviderConfig) -> Result<Self> {
        let config = config.resolved()?;
        let profile = config.profile();
        Ok(Self {
            profile,
            client: build_http_client(config.request_timeout_secs)?,
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
        let chunks = self
            .collect_chat_chunks(endpoint, "/chat/completions", &body.model, &body, false)
            .await?;
        Ok(Box::pin(stream::iter(
            chunks.into_iter().map(Ok::<ProviderChunk, anyhow::Error>),
        )))
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
        let chunks = self
            .collect_completion_chunks(
                DeepSeekEndpointClass::Beta,
                "/completions",
                &body.model,
                &body,
            )
            .await?;
        Ok(Box::pin(stream::iter(
            chunks.into_iter().map(Ok::<ProviderChunk, anyhow::Error>),
        )))
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
                target: "termquill_provider_deepseek",
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
            true,
        )
        .await
    }

    async fn collect_chat_chunks<T: Serialize>(
        &self,
        endpoint: DeepSeekEndpointClass,
        path: &str,
        model_name: &str,
        body: &T,
        retry_on_reasoning_400: bool,
    ) -> Result<Vec<ProviderChunk>> {
        let mut stream = self
            .stream_chat_chunks(endpoint, path, model_name, body, retry_on_reasoning_400)
            .await?;
        let mut chunks = Vec::new();
        while let Some(chunk) = stream.next().await {
            chunks.push(chunk?);
        }
        Ok(chunks)
    }

    async fn stream_chat_chunks<T: Serialize>(
        &self,
        endpoint: DeepSeekEndpointClass,
        path: &str,
        model_name: &str,
        body: &T,
        retry_on_reasoning_400: bool,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let mut attempts = 0usize;
        loop {
            attempts += 1;
            let url = format!("{}{}", self.base_url_for_endpoint(endpoint), path);
            let response = self
                .post_json(&url, body)
                .await
                .context("deepseek request failed")?;
            let status = response.status();

            if !status.is_success() {
                let error_body = response.text().await.unwrap_or_default();
                let classified = classify_status(status.as_u16(), &error_body);
                let retryable = matches!(
                    classified,
                    DeepSeekProviderError::RateLimited | DeepSeekProviderError::RetryableStatus(_)
                ) || (retry_on_reasoning_400
                    && status.as_u16() == 400
                    && error_body.contains("reasoning_content"));
                if retryable && attempts < 2 {
                    warn!(
                        "retrying deepseek request after status {} body {}",
                        status.as_u16(),
                        error_body
                    );
                    sleep(Duration::from_millis(100)).await;
                    continue;
                }
                return Err(classified.into());
            }

            return Ok(chat_response_stream(response, model_name.to_owned()));
        }
    }

    async fn collect_completion_chunks<T: Serialize>(
        &self,
        endpoint: DeepSeekEndpointClass,
        path: &str,
        model_name: &str,
        body: &T,
    ) -> Result<Vec<ProviderChunk>> {
        let url = format!("{}{}", self.base_url_for_endpoint(endpoint), path);
        let response = self
            .post_json(&url, body)
            .await
            .context("deepseek completion request failed")?;
        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(classify_status(status.as_u16(), &error_body).into());
        }

        let mut chunks = Vec::new();
        let mut decoder = DeepSeekSseDecoder::default();
        let mut byte_stream = response.bytes_stream();
        while let Some(next) = byte_stream.next().await {
            let bytes = next.context("failed to read completion chunk")?;
            let raw =
                String::from_utf8(bytes.to_vec()).context("invalid UTF-8 completion chunk")?;
            for frame in decoder.push(&raw)? {
                match frame {
                    crate::response::DeepSeekSseFrame::Blank
                    | crate::response::DeepSeekSseFrame::Comment => {}
                    crate::response::DeepSeekSseFrame::Done => chunks.push(ProviderChunk::Done),
                    crate::response::DeepSeekSseFrame::Data(data) => {
                        let envelope: DeepSeekCompletionStreamEnvelope =
                            serde_json::from_str(&data).with_context(|| {
                                format!(
                                    "invalid DeepSeek completion event {}",
                                    truncate_event_payload(&data)
                                )
                            })?;
                        for choice in envelope.choices {
                            if let Some(text) = choice.text {
                                chunks.push(ProviderChunk::TextDelta(text));
                            }
                            if matches!(choice.finish_reason.as_deref(), Some("stop")) {
                                chunks.push(ProviderChunk::Done);
                            }
                        }
                        if let Some(usage) = envelope.usage {
                            chunks.push(ProviderChunk::Usage(crate::pricing::enrich_usage_costs(
                                model_name,
                                termquill_kernel::UsageStats {
                                    prompt_tokens: usage.prompt_tokens,
                                    completion_tokens: usage.completion_tokens,
                                    cache_hit_tokens: usage
                                        .prompt_cache_hit_tokens
                                        .unwrap_or_default(),
                                    cache_miss_tokens: usage
                                        .prompt_cache_miss_tokens
                                        .unwrap_or_default(),
                                    input_cost: 0.0,
                                    output_cost: 0.0,
                                    cache_savings: 0.0,
                                    system_fingerprint: envelope.system_fingerprint.clone(),
                                },
                            )));
                        }
                    }
                }
            }
        }
        for frame in decoder.finish()? {
            match frame {
                crate::response::DeepSeekSseFrame::Blank
                | crate::response::DeepSeekSseFrame::Comment => {}
                crate::response::DeepSeekSseFrame::Done => chunks.push(ProviderChunk::Done),
                crate::response::DeepSeekSseFrame::Data(data) => {
                    let envelope: DeepSeekCompletionStreamEnvelope = serde_json::from_str(&data)
                        .with_context(|| {
                            format!(
                                "invalid DeepSeek completion event {}",
                                truncate_event_payload(&data)
                            )
                        })?;
                    for choice in envelope.choices {
                        if let Some(text) = choice.text {
                            chunks.push(ProviderChunk::TextDelta(text));
                        }
                        if matches!(choice.finish_reason.as_deref(), Some("stop")) {
                            chunks.push(ProviderChunk::Done);
                        }
                    }
                    if let Some(usage) = envelope.usage {
                        chunks.push(ProviderChunk::Usage(crate::pricing::enrich_usage_costs(
                            model_name,
                            termquill_kernel::UsageStats {
                                prompt_tokens: usage.prompt_tokens,
                                completion_tokens: usage.completion_tokens,
                                cache_hit_tokens: usage.prompt_cache_hit_tokens.unwrap_or_default(),
                                cache_miss_tokens: usage
                                    .prompt_cache_miss_tokens
                                    .unwrap_or_default(),
                                input_cost: 0.0,
                                output_cost: 0.0,
                                cache_savings: 0.0,
                                system_fingerprint: envelope.system_fingerprint.clone(),
                            },
                        )));
                    }
                }
            }
        }
        if !chunks
            .iter()
            .any(|chunk| matches!(chunk, ProviderChunk::Done))
        {
            chunks.push(ProviderChunk::Done);
        }
        Ok(chunks)
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

fn chat_response_stream(
    response: reqwest::Response,
    model_name: String,
) -> Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> {
    let byte_stream = response.bytes_stream();
    let decoder = DeepSeekSseDecoder::default();
    let mapper = StreamMapper::new(model_name);
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
                    match enqueue_chat_frames(&mut state.1, &mut state.2, &mut state.3, &raw) {
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
                None => {
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

fn enqueue_chat_frames(
    decoder: &mut DeepSeekSseDecoder,
    mapper: &mut StreamMapper,
    pending: &mut VecDeque<ProviderChunk>,
    raw: &str,
) -> Result<bool> {
    let mut done_seen = false;
    for frame in decoder.push(raw)? {
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

#[cfg(test)]
#[path = "tests/provider_tests.rs"]
mod tests;
